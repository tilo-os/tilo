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

use collab_types::caps::Actor;
use tilo_desktop_model::Workspace;

use tilo_actions::remote::{
    APPLICATION_CLOSE_ACTION, APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX,
    ActionHostError, Caller, CallerPrincipal, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, Completer, Confirmation, FreshActionBinding,
    InvocationReceipt, InvocationState, InvokeRequest, OWN_PROVIDER, Pending, ReceiptStatus,
    Reservation, application_provider, pending, workspace_provider, workspace_provider_room,
};
use tilo_actions::{ActionAddress, ActionDescriptor, Collects, ExecutionLocus, InputSpec};

use super::launchable::{is_launchable_provider, launchable_provider};
use super::{
    ActionDirectoryBridgeError, ActionDirectoryBridgeReply, ActionDirectoryBridgeResponse,
    ActionDirectoryRoot, BrokerActor, CapDecision, CapnpBrokerState, DIRECTORY_MAX_PARAMS_BYTES,
    InvocationInputs, InvocationOwner, LaunchScope, PublishedAction, PublishedInput,
    PublishedSurface, ResourceRef, Verb, confirmation_satisfied, permission_verb,
    published_directory_action, validate_directory_target_input,
};
use crate::automation;
use crate::collab_broker::CrdtRoute;
use tilo_automation::SHORTCUTS_APP_ID;

/// The longest a waiter on a running invocation sleeps without a signalled
/// change, so a change nothing signals (an idle expiry) costs latency, never
/// a stalled shortcut run.
const CHANGE_SAFETY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

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
fn check_execution(caller: &Caller) -> Result<(), ActionHostError> {
    if caller.execution_id().is_some() && !caller.execution_is_live() {
        return Err(ActionHostError::Unavailable {
            reason: "execution session ended".into(),
        });
    }
    Ok(())
}

fn report_job_binding_refusal(outcome: &'static str) {
    tracing::debug!(
        target: "tilo_native_governance",
        stage = "certified_job_binding",
        outcome,
        "certified job binding observation refused"
    );
}

enum DirectoryResolutionMode {
    Revision(String),
    Fresh(Box<FreshActionBinding>),
}

struct ValidatedDirectoryInvocation {
    mode: DirectoryResolutionMode,
    argument: Option<String>,
}

fn validate_directory_invocation(
    root: &ActionDirectoryRoot,
    request: &InvokeRequest,
) -> Result<ValidatedDirectoryInvocation, ActionHostError> {
    if request.revision.is_some() && request.fresh_binding.is_some() {
        return Err(ActionHostError::InvalidRequest {
            reason: "fresh and revision-bound directory modes cannot be mixed".into(),
        });
    }
    let mode = if let Some(revision) = request
        .revision
        .clone()
        .filter(|revision| !revision.is_empty())
    {
        DirectoryResolutionMode::Revision(revision)
    } else if let Some(binding) = request.fresh_binding.clone() {
        let descriptor = &binding.descriptor;
        let identity = descriptor.semantic_identity.as_ref();
        if descriptor.address != request.target.address
            || identity.is_none_or(|identity| {
                !identity.is_valid() || identity.kind != descriptor.resource_kind
            })
        {
            return Err(ActionHostError::InvalidRequest {
                reason: "the fresh directory binding does not name the exact semantic target"
                    .into(),
            });
        }
        DirectoryResolutionMode::Fresh(Box::new(binding))
    } else {
        return Err(ActionHostError::InvalidRequest {
            reason: "a directory action needs exactly one pinned revision or fresh binding".into(),
        });
    };
    let argument = request.argument.to_slot();
    if let Some(reason) = argument.as_deref().and_then(oversized_argument) {
        return Err(ActionHostError::InvalidRequest { reason });
    }
    let revision = match &mode {
        DirectoryResolutionMode::Revision(revision) => revision.as_str(),
        DirectoryResolutionMode::Fresh(_) => "",
    };
    let address = &request.target.address;
    validate_directory_target_input(
        &root.cluster_id,
        revision,
        &address.resource,
        &address.action,
        argument.as_deref(),
        None,
    )
    .map_err(|_| ActionHostError::InvalidRequest {
        reason: "the directory target or argument is malformed".into(),
    })?;
    Ok(ValidatedDirectoryInvocation { mode, argument })
}

struct LaunchDestination {
    workspace: Workspace,
    app_id: String,
    record: Option<String>,
}

impl BrokerActor {
    /// The broker actor a caller is admitted as.
    #[must_use]
    pub(super) fn for_caller(caller: &Caller) -> Self {
        match caller.principal() {
            CallerPrincipal::Person(id) => Self::person(id.clone()),
            CallerPrincipal::Agent(id) => Self::agent(format!("agent:{id}")),
            CallerPrincipal::App(id) => Self::agent(format!("app:{id}")),
        }
    }
}

/// The broker as the [`ActionHost`](tilo_actions::remote::ActionHost) an
/// admitted application or agent connection reaches over its channel's
/// `remoteAction` door. [`OWN_PROVIDER`] names that connection itself.
pub(super) struct ConnectionActionHost {
    pub(super) state: Arc<CapnpBrokerState>,
    pub(super) own_peer: u64,
    /// The connection's revalidated admission: an application's own effect,
    /// directed back to it, runs under this original caller's custody.
    pub(super) origin: super::published_invocation::InvocationCaller,
}

impl tilo_actions::remote::ActionHost for ConnectionActionHost {
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
        self.state.action_observe(caller, id)
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
    ) -> Result<(), ActionHostError> {
        self.state.action_cancel(caller, id)
    }
}

/// The broker's own door, as the [`ActionHost`](tilo_actions::remote::ActionHost)
/// a shortcut run invokes its steps through. It stands for no connection:
/// every step is admitted for the run's caller alone.
pub(super) struct BrokerDoor {
    state: Arc<CapnpBrokerState>,
    /// The invocation changes the run had seen when it last observed, so
    /// a change between that observation and its next wait is not missed.
    seen: std::sync::Mutex<tokio::sync::watch::Receiver<u64>>,
}

impl BrokerDoor {
    pub(super) fn new(state: Arc<CapnpBrokerState>) -> Self {
        let seen = std::sync::Mutex::new(state.invocation_changes());
        Self { state, seen }
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
        self.state.action_invoke(caller, None, None, &request)
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
    ) -> Result<(), ActionHostError> {
        self.state.action_cancel(caller, id)
    }
}

fn descriptor(action: &PublishedAction) -> ActionDescriptor {
    ActionDescriptor {
        semantic_identity: action.semantic_identity.clone(),
        address: ActionAddress::new(&action.resource, &action.action),
        resource_kind: action.resource_kind.clone(),
        resource_label: action.resource_label.clone(),
        action_label: action.action_label.clone(),
        description: action.description.clone(),
        // The host runs the action wherever its app lives; a published
        // surface carries no locus of its own.
        locus: ExecutionLocus::default(),
        kind: action.operation_kind,
        contract: action.contract.clone(),
        input: match &action.input {
            PublishedInput::None => InputSpec::None,
            PublishedInput::Text {
                prompt,
                placeholder,
                wants_content,
            } => InputSpec::Text {
                prompt: prompt.clone(),
                placeholder: placeholder.clone(),
                collects: if *wants_content {
                    Collects::Content
                } else {
                    Collects::Words
                },
            },
        },
        params: action.params.clone(),
        view: action.view.clone(),
    }
}

pub(super) fn validate_published_argument(
    action: &PublishedAction,
    argument: Option<&str>,
) -> Result<(), String> {
    match argument {
        None => tilo_actions::ActionArgs::bind(
            &action.params,
            std::iter::empty::<(String, tilo_actions::ArgValue)>(),
        )
        .map(|_| ())
        .map_err(|_| {
            "action_invalid_arguments: the arguments do not match the published contract".into()
        }),
        Some(argument) if argument.starts_with(tilo_actions::ARGUMENTS_JSON_PREFIX) => {
            if action.params.is_empty() {
                return Err(
                    "action_invalid_arguments: the published action takes no arguments".into(),
                );
            }
            let object = argument
                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
                .and_then(|value| value.as_object().cloned())
                .ok_or("action_invalid_arguments: expected a typed argument object")?;
            tilo_actions::ActionArgs::bind_json(&action.params, &object)
                .map(|_| ())
                .map_err(|_| {
                    "action_invalid_arguments: the arguments do not match the published contract"
                        .into()
                })
        }
        Some(_) if matches!(&action.input, PublishedInput::Text { .. }) => Ok(()),
        Some(_) => Err("action_invalid_arguments: expected typed arguments".into()),
    }
}

fn runtime_fresh_binding(binding: &FreshActionBinding) -> runtime_collab::FreshActionBinding {
    let descriptor = &binding.descriptor;
    runtime_collab::FreshActionBinding {
        descriptor: runtime_collab::ExportedAction::from(descriptor),
        locus: serde_json::to_string(&descriptor.locus).unwrap_or_default(),
        operation_type: binding.operation_type.clone(),
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
            "applications.app",
            "Launch this application into the current workspace",
        )
    };
    ActionDescriptor {
        semantic_identity: None,
        address: ActionAddress::new(
            format!("{APPLICATION_RESOURCE_PREFIX}{}", app.package_id),
            APPLICATION_OPEN_ACTION,
        ),
        resource_kind: resource_kind.into(),
        resource_label: display_name.clone(),
        action_label: format!("Open {display_name}"),
        description: Some(description.into()),
        locus: ExecutionLocus::default(),
        kind: tilo_actions::OperationKind::Effect,
        contract: tilo_actions::ActionContract::default(),
        // An invocation may carry the content hash of bytes to open
        // read-only as its text argument; without one the app opens empty.
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

fn app_close_descriptor(package_id: &str, display_name: &str) -> ActionDescriptor {
    ActionDescriptor {
        semantic_identity: None,
        address: ActionAddress::new(
            format!("{APPLICATION_RESOURCE_PREFIX}{package_id}"),
            APPLICATION_CLOSE_ACTION,
        ),
        resource_kind: "applications.app".into(),
        resource_label: display_name.to_string(),
        action_label: format!("Close {display_name}"),
        description: Some(
            "Close this application's windows in the current workspace; opening it again \
             starts it with the permissions it holds now"
                .into(),
        ),
        locus: ExecutionLocus::default(),
        kind: tilo_actions::OperationKind::Effect,
        contract: tilo_actions::ActionContract {
            permission: tilo_actions::PermissionRequirement::Write,
            risk: tilo_actions::RiskLevel::Low,
            retry: tilo_actions::RetryPolicy::Idempotent,
            ..tilo_actions::ActionContract::default()
        },
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
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
fn refusal(reason: String) -> ActionHostError {
    if reason.starts_with("action_confirmation_required")
        || reason.contains("confirmation is required")
    {
        ActionHostError::ConfirmationRequired
    } else if let Some(reason) = reason.strip_prefix("action_invalid_arguments: ") {
        ActionHostError::InvalidRequest {
            reason: reason.to_owned(),
        }
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

/// The legacy JSON evidence the workspace profile path reads.
fn confirmation_evidence(confirmation: Option<&Confirmation>) -> serde_json::Value {
    match confirmation {
        None => serde_json::json!({}),
        Some(Confirmation::Confirmed) => serde_json::json!({"confirm": true}),
        Some(Confirmation::Phrase(phrase)) => serde_json::json!({"confirmation_phrase": phrase}),
    }
}

impl CapnpBrokerState {
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
    /// role and has only what the workspace grants it.
    ///
    /// # Errors
    ///
    /// The refusal to answer with, when the request is outside what the app
    /// declared.
    pub(crate) fn app_door(
        &self,
        person: &Caller,
        app_id: &str,
        message: &tilo_actions::remote::RemoteActionMessage,
    ) -> Result<Caller, Box<tilo_actions::remote::RemoteActionMessage>> {
        use tilo_actions::remote::{RemoteActionMessage, refuse};
        let access = self.app_action_access(app_id);
        let mut as_app = Caller::local_app(app_id);
        if let Some(room) = person.room() {
            as_app = as_app.in_room(room);
        }
        let permitted = match message {
            RemoteActionMessage::CatalogueRequest { .. } => access.query.then(|| person.clone()),
            RemoteActionMessage::Invoke(request) => {
                if self.app_owns(person, app_id, request) {
                    Some(person.clone())
                } else {
                    access.invoke.then_some(as_app)
                }
            }
            RemoteActionMessage::Observe { invocation }
            | RemoteActionMessage::Cancel { invocation } => {
                Some(if self.remote_job(&as_app, invocation).is_some() {
                    as_app
                } else {
                    person.clone()
                })
            }
            // Records are read device to device by the shell; an application
            // does not reach another device's records through this door.
            RemoteActionMessage::RecordRequest { .. } => None,
            RemoteActionMessage::CatalogueResponse { .. }
            | RemoteActionMessage::Receipt(_)
            | RemoteActionMessage::Update { .. }
            | RemoteActionMessage::CancelResponse { .. }
            | RemoteActionMessage::RecordResponse { .. } => Some(person.clone()),
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
            || (workspace_provider_room(&target.provider).is_some()
                && target
                    .address
                    .resource
                    .starts_with(APPLICATION_RESOURCE_PREFIX)
                && target.address.action == APPLICATION_OPEN_ACTION)
        {
            return true;
        }
        if let Some(row) = self
            .eager_catalogue(person, true)
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

    /// Workspaces `caller` may act in, with the policy actor for each. A
    /// caller confined to a room was admitted to it by its connection; any
    /// other caller must be a member.
    fn workspaces_for(&self, caller: &Caller) -> Vec<(Workspace, Actor, u64)> {
        let Some(membership) = self.membership.as_ref() else {
            return Vec::new();
        };
        // One policy clock sample for the whole pass; an unavailable clock
        // offers nothing rather than deciding at an unknown time.
        let Ok(now) = self.policy_now_ns().map(|now| now / 1_000_000_000) else {
            return Vec::new();
        };
        let Ok(tree) = membership.snapshot() else {
            return Vec::new();
        };
        tree.workspaces
            .into_iter()
            .filter(|workspace| {
                let room = workspace.room.to_string();
                match caller.room() {
                    Some(confined) => confined == room,
                    None => match caller.principal() {
                        CallerPrincipal::Person(id) => workspace.role_of_at(id, now).is_some(),
                        CallerPrincipal::Agent(id) => workspace
                            .role_of_agent_in_room_at(id, &workspace.room.to_string(), now)
                            .is_some(),
                        // An application is always admitted into one room;
                        // unconfined, it is a member of none.
                        CallerPrincipal::App(_) => false,
                    },
                }
            })
            .map(|workspace| {
                let policy = self
                    .caller_actor(caller, &workspace.room.to_string())
                    .policy_in(&workspace, now);
                (workspace, policy, now)
            })
            .collect()
    }

    pub(super) fn allowed(
        workspace: &Workspace,
        policy: &Actor,
        now: u64,
        app: &str,
        resource: &str,
        action: &str,
        permission: tilo_actions::PermissionRequirement,
    ) -> bool {
        // An action that asks nothing of a person still has to be granted to
        // an application: it holds no membership that would imply it.
        let acting_app = policy
            .principals
            .iter()
            .any(|principal| matches!(principal, collab_types::caps::Principal::App(_)));
        let verb = permission_verb(permission).or_else(|| acting_app.then_some(Verb::Read));
        verb.is_none_or(|verb| {
            workspace.decide_at(
                policy,
                &ResourceRef::Action {
                    app: app.to_string(),
                    resource: resource.to_string(),
                    action: action.to_string(),
                },
                verb,
                now,
            ) == CapDecision::Allow
        })
    }

    /// Every live published surface, carrying its actions only when
    /// `with_actions`: who publishes is copied, not every app's whole
    /// surface, when that is all a caller asked.
    fn live_surfaces(&self, with_actions: bool) -> Vec<PublishedSurface> {
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
                        actions: if with_actions {
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
    /// Every eager row `caller` is offered: live application actions its
    /// role may invoke, the workspace's own governance actions, and — for a
    /// caller on this machine — launching an application it may open. An
    /// action the caller may not use is absent. Alongside them, the
    /// publishers the caller may browse or invoke.
    pub(super) fn eager_catalogue(
        &self,
        caller: &Caller,
        with_rows: bool,
    ) -> (Vec<CatalogueRow>, Vec<CatalogueProvider>) {
        // Retained surfaces of closed apps are the launchable catalogue's,
        // and a providers-only answer needs no live surface's actions.
        let surfaces = self.live_surfaces(with_rows);
        let roots = self.action_directory_roots();
        let task_peers = self
            .workspace_task_peers
            .lock()
            .map(|peers| peers.clone())
            .unwrap_or_default();
        // Listed once for every workspace's open and close rows.
        let applications = self
            .services
            .launcher
            .as_ref()
            .filter(|_| with_rows && caller.room().is_some())
            .map(|launcher| launcher.applications())
            .unwrap_or_default();
        let closes_applications = self
            .services
            .launcher
            .as_ref()
            .is_some_and(|launcher| launcher.closes_applications());
        let mut rows = Vec::new();
        let mut providers = Vec::new();
        // Keyboard shortcuts are this device's own settings: offered to
        // callers on this machine, never to a remote device.
        let keyboard = self
            .services
            .keyboard_shortcuts
            .as_ref()
            .filter(|_| with_rows && caller.device_public_key().is_none())
            .map(|service| (service, settings_store::hardware::ShortcutConfig::load()));
        for (workspace, policy, now) in self.workspaces_for(caller) {
            let room = workspace.room.to_string();
            let mut live_apps = std::collections::HashSet::new();
            for surface in surfaces
                .iter()
                .filter(|surface| surface.live && surface.route.cluster_id == room)
            {
                let provider = if task_peers.get(&room) == Some(&surface.peer_id) {
                    workspace_provider(&room)
                } else {
                    surface.peer_id.to_string()
                };
                live_apps.insert(surface.app_id.clone());
                providers.push(CatalogueProvider {
                    provider: provider.clone(),
                    app_id: surface.app_id.clone(),
                    room: room.clone(),
                    record_id: surface.route.record_id.clone(),
                    browsable: roots.iter().any(|root| root.peer_id == surface.peer_id),
                });
                if !with_rows {
                    continue;
                }
                rows.extend(
                    surface
                        .actions
                        .iter()
                        .filter(|action| {
                            Self::allowed(
                                &workspace,
                                &policy,
                                now,
                                &surface.app_id,
                                &action.resource,
                                &action.action,
                                action.contract.permission,
                            )
                        })
                        .map(|action| {
                            action_row(provider.clone(), &surface.app_id, descriptor(action))
                        }),
                );
            }
            if caller.room().is_some() {
                self.launchable_catalogue(
                    &workspace,
                    &policy,
                    now,
                    &live_apps,
                    with_rows.then_some(&mut rows),
                    &mut providers,
                );
            }
            let unlisted = roots
                .iter()
                .filter(|root| {
                    root.cluster_id == room
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
                    room: room.clone(),
                    record_id: root.record_id.clone(),
                    browsable: true,
                });
            }
            if !with_rows {
                continue;
            }
            if let Some(governance) = self.services.workspace_governance.as_ref() {
                rows.extend(
                    governance
                        .descriptors(&workspace)
                        .into_iter()
                        .filter(|descriptor| {
                            Self::allowed(
                                &workspace,
                                &policy,
                                now,
                                governance.app_id(),
                                &descriptor.address.resource,
                                &descriptor.address.action,
                                descriptor.contract.permission,
                            )
                        })
                        .map(|descriptor| {
                            action_row(workspace_provider(&room), governance.app_id(), descriptor)
                        }),
                );
            }
            if self.services.shortcut_library.is_some() {
                rows.extend(
                    automation::descriptors(&workspace)
                        .into_iter()
                        .filter(|descriptor| {
                            Self::allowed(
                                &workspace,
                                &policy,
                                now,
                                SHORTCUTS_APP_ID,
                                &descriptor.address.resource,
                                &descriptor.address.action,
                                descriptor.contract.permission,
                            )
                        })
                        .map(|descriptor| {
                            action_row(workspace_provider(&room), SHORTCUTS_APP_ID, descriptor)
                        }),
                );
            }
            if let Some((service, keymap)) = &keyboard {
                rows.extend(
                    service
                        .catalogue_rows(&workspace_provider(&room), keymap)
                        .into_iter()
                        .filter(|row| {
                            row.action.as_ref().is_some_and(|descriptor| {
                                Self::allowed(
                                    &workspace,
                                    &policy,
                                    now,
                                    service.app_id(),
                                    &descriptor.address.resource,
                                    &descriptor.address.action,
                                    descriptor.contract.permission,
                                )
                            })
                        }),
                );
            }
            if caller.room().is_some() {
                rows.extend(
                    applications
                        .iter()
                        .filter(|app| !live_apps.contains(&app.package_id))
                        .filter(|app| {
                            workspace.decide_at(
                                &policy,
                                &ResourceRef::App(app.package_id.clone()),
                                Verb::Write,
                                now,
                            ) == CapDecision::Allow
                        })
                        .map(|app| {
                            action_row(
                                workspace_provider(&room),
                                &app.package_id,
                                app_open_descriptor(app),
                            )
                        }),
                );
                rows.extend(
                    applications
                        .iter()
                        .filter(|_| closes_applications)
                        .filter(|app| live_apps.contains(&app.package_id))
                        .filter(|app| {
                            workspace.decide_at(
                                &policy,
                                &ResourceRef::App(app.package_id.clone()),
                                Verb::Write,
                                now,
                            ) == CapDecision::Allow
                        })
                        .map(|app| {
                            action_row(
                                workspace_provider(&room),
                                &app.package_id,
                                app_close_descriptor(&app.package_id, &app.display_name),
                            )
                        }),
                );
            }
        }
        (rows, providers)
    }

    /// Actions retained from applications that are closed in `workspace`,
    /// offered to a caller on this machine that may launch the application
    /// and use the action. Invoking one launches the app first.
    fn launchable_catalogue(
        &self,
        workspace: &Workspace,
        policy: &Actor,
        now: u64,
        live_apps: &std::collections::HashSet<String>,
        mut rows: Option<&mut Vec<CatalogueRow>>,
        providers: &mut Vec<CatalogueProvider>,
    ) {
        let room = workspace.room.to_string();
        for surface in self.launchable.surfaces().into_iter().filter(|surface| {
            surface.route.cluster_id == room
                && !live_apps.contains(&surface.app_id)
                && workspace.decide_at(
                    policy,
                    &ResourceRef::App(surface.app_id.clone()),
                    Verb::Write,
                    now,
                ) == CapDecision::Allow
        }) {
            let provider = launchable_provider(&surface);
            if let Some(rows) = rows.as_deref_mut() {
                rows.extend(
                    surface
                        .actions
                        .iter()
                        .filter(|action| {
                            Self::allowed(
                                workspace,
                                policy,
                                now,
                                &surface.app_id,
                                &action.resource,
                                &action.action,
                                action.contract.permission,
                            )
                        })
                        .map(|action| {
                            action_row(provider.clone(), &surface.app_id, descriptor(action))
                        }),
                );
            }
            providers.push(CatalogueProvider {
                provider,
                app_id: surface.app_id.clone(),
                room: room.clone(),
                record_id: surface.route.record_id.clone(),
                browsable: false,
            });
        }
    }

    /// The directory root publishing `peer_id` in a workspace `caller` may
    /// act in, with the actor the caller is admitted as there.
    fn directory_root_for(&self, caller: &Caller, peer_id: u64) -> Option<ActionDirectoryRoot> {
        let rooms = self
            .workspaces_for(caller)
            .into_iter()
            .map(|(workspace, _, _)| workspace.room.to_string())
            .collect::<Vec<_>>();
        self.action_directory_roots()
            .into_iter()
            .find(|root| root.peer_id == peer_id && rooms.contains(&root.cluster_id))
    }

    /// One page of what `caller` is offered. A provider publishing a live
    /// directory is browsed there, on the broker's local set; everything
    /// else is answered at once from the eager surfaces.
    pub fn action_query(
        self: &Arc<Self>,
        caller: &Caller,
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
            .and_then(|provider| provider.parse::<u64>().ok())
            .and_then(|peer_id| self.directory_root_for(caller, peer_id))
        {
            return self.query_directory(caller, &root, query);
        }
        // `max_results: 0` asks only who publishes; skip building rows.
        let providers_only = query.provider.is_none() && query.max_results == 0;
        let (mut rows, providers) = self.eager_catalogue(caller, !providers_only);
        if let Some(provider) = &query.provider {
            rows.retain(|row| &row.provider == provider);
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

    fn canonical_provider_page(
        &self,
        caller: &Caller,
        query: &CatalogueQuery,
    ) -> Result<CataloguePage, ActionHostError> {
        let route = query
            .canonical_route
            .as_ref()
            .ok_or(ActionHostError::UnknownAction)?;
        if query.max_results != 0
            || query.provider.is_some()
            || query.view_resource.is_some()
            || query.parent_resource.is_some()
            || !query.needle.is_empty()
            || query.target.is_some()
            || query.revision.is_some()
            || !query.cursor.is_empty()
            || query.limits.is_some()
        {
            return Err(ActionHostError::InvalidRequest {
                reason: "a canonical route lookup must request only its provider".into(),
            });
        }
        if !self
            .workspaces_for(caller)
            .iter()
            .any(|(workspace, _, _)| workspace.room.to_string() == route.room)
        {
            return Err(ActionHostError::NotPermitted);
        }
        let mut page = CataloguePage {
            exhausted: true,
            revision: "canonical-route-v1".into(),
            ..CataloguePage::default()
        };
        if let Some((root, token)) = self.canonical_directory(route)? {
            page.providers.push(CatalogueProvider {
                provider: token,
                app_id: root.app_id,
                room: root.cluster_id,
                record_id: root.record_id,
                browsable: true,
            });
        }
        Ok(page)
    }

    fn query_directory(
        &self,
        caller: &Caller,
        root: &ActionDirectoryRoot,
        query: &CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        let actor = self.caller_actor(caller, &root.cluster_id);
        let queue = self.action_directory_request_queue();
        let resolution_revision = query.target.as_ref().and_then(|_| {
            query
                .revision
                .clone()
                .filter(|revision| !revision.is_empty())
        });
        let reply = if let Some(target) = &query.target {
            let Some(revision) = resolution_revision.clone() else {
                return Pending::ready(Err(ActionHostError::InvalidRequest {
                    reason: "resolving a target needs the revision it was chosen at".into(),
                }));
            };
            queue.try_resolve_pending(
                actor,
                &root.cluster_id,
                root.peer_id,
                revision,
                &target.resource,
                &target.action,
            )
        } else {
            let limits = query.limits.unwrap_or_default();
            queue.try_query_pending(
                actor,
                &root.cluster_id,
                runtime_collab::ActionDirectoryQuery {
                    peer_id: root.peer_id,
                    revision: query.revision.clone().unwrap_or_default(),
                    cursor: query.cursor.clone(),
                    needle: query.needle.clone(),
                    parent_resource: query.parent_resource.clone().unwrap_or_default(),
                    max_results: u32::try_from(query.page_size()).unwrap_or(1),
                    // The broker clamps every budget to its own maxima.
                    max_bytes: limits.max_bytes.unwrap_or(u32::MAX),
                    max_work: limits.max_work.unwrap_or(u32::MAX),
                    max_depth: limits.max_depth.unwrap_or(u16::MAX),
                },
            )
        };
        let root = root.clone();
        let caller = caller.clone();
        match reply {
            Ok(reply) => reply.map(move |reply| {
                check_execution(&caller)?;
                directory_page(&root, reply, resolution_revision.as_deref())
            }),
            Err(error) => Pending::ready(Err(bridge_error(error))),
        }
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
        if let Err(error) = check_execution(caller) {
            return Pending::ready(Err(error));
        }
        if request.revision.is_some() && request.fresh_binding.is_some() {
            return Pending::ready(Err(ActionHostError::InvalidRequest {
                reason: "fresh and revision-bound directory modes cannot be mixed".into(),
            }));
        }
        if request.fresh_binding.is_some()
            && !request
                .target
                .provider
                .starts_with(super::canonical_route::PROVIDER_PREFIX)
        {
            return if let Some(root) = self.directory_target(caller, request) {
                self.invoke_directory(caller, root, origin, request)
            } else {
                Pending::ready(Err(ActionHostError::InvalidRequest {
                    reason: "fresh binding is available only for a live action directory".into(),
                }))
            };
        }
        if request
            .target
            .provider
            .starts_with(super::canonical_route::PROVIDER_PREFIX)
        {
            // Exact UUID/request replay precedes live selection checks. A retired
            // custodian must not turn an admitted request into another dispatch.
            if let Some(job) = self.remote_job(caller, &request.invocation) {
                let replay = self
                    .remote_invocations
                    .lock()
                    .map_err(|_| ActionHostError::Unavailable {
                        reason: "the invocation ledger is unavailable".into(),
                    })
                    .and_then(|mut ledger| ledger.reserve(caller, request));
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
        {
            return self.invoke_launchable(caller, origin, request);
        }
        if let Some(root) = self.directory_target(caller, request) {
            return self.invoke_directory(caller, root, origin, request);
        }
        if self.services.shortcut_library.is_some()
            && let Some(room) = workspace_provider_room(&request.target.provider)
            && let Some(shortcut) = automation::run_target(&request.target.address)
        {
            return Pending::ready(self.invoke_shortcut_run(caller, room, shortcut, request));
        }
        Pending::ready(self.invoke_eager(caller, own_peer, launch, origin, request))
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
        let reservation = match self.remote_invocations.lock() {
            Ok(mut ledger) => ledger.reserve(caller, request),
            Err(_) => Err(ActionHostError::Unavailable {
                reason: "the invocation ledger is unavailable".into(),
            }),
        };
        match reservation {
            Err(error) => return Pending::ready(Err(error)),
            Ok(Reservation::Admitted) => {
                return Pending::ready(
                    self.remote_job(caller, &request.invocation)
                        .map(|job| self.receipt_for(request.invocation, job))
                        .ok_or(ActionHostError::UnknownInvocation),
                );
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
                .relaunch_and_deliver(&completer, &caller, &destination, origin.as_ref(), &request)
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
        let address = &request.target.address;
        let (room, app_id, record, offered) =
            if let Some((room, app_id)) = application_provider(&request.target.provider) {
                let offered = self
                    .published_surfaces()
                    .into_iter()
                    .chain(self.launchable.surfaces())
                    .filter(|surface| surface.app_id == app_id && surface.route.cluster_id == room)
                    .find_map(|surface| {
                        surface.actions.into_iter().find(|action| {
                            action.resource == address.resource && action.action == address.action
                        })
                    });
                (room.to_owned(), app_id.to_owned(), None, offered)
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
        if caller.room().is_none() {
            return Err(ActionHostError::UnknownAction);
        }
        let (workspace, policy, now) = self
            .workspaces_for(caller)
            .into_iter()
            .find(|(workspace, _, _)| workspace.room.to_string() == room)
            .ok_or(ActionHostError::UnknownAction)?;
        if workspace.decide_at(&policy, &ResourceRef::App(app_id.clone()), Verb::Write, now)
            != CapDecision::Allow
        {
            return Err(refusal(
                "the caller is not allowed to launch that application".into(),
            ));
        }
        if let Some(action) = offered {
            if !Self::allowed(
                &workspace,
                &policy,
                now,
                &app_id,
                &action.resource,
                &action.action,
                action.contract.permission,
            ) {
                return Err(ActionHostError::UnknownAction);
            }
            if !confirmation_satisfied(&action, request.confirmation.as_ref()) {
                return Err(ActionHostError::ConfirmationRequired);
            }
        }
        Ok(LaunchDestination {
            workspace,
            app_id,
            record,
        })
    }

    /// Launch `app_id` unless it already publishes the action, then deliver
    /// the invocation to the connection that does, through the same
    /// admission a live action takes.
    ///
    /// The wait wakes on what readiness depends on — a surface published or
    /// withdrawn, an invocation stream opened or closed, in either order —
    /// on the delivery deadline, and on the caller dropping its answer.
    async fn relaunch_and_deliver(
        self: &Arc<Self>,
        completer: &Completer<Result<InvocationReceipt, ActionHostError>>,
        caller: &Caller,
        destination: &LaunchDestination,
        origin: Option<&super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
    ) -> Result<u64, ActionHostError> {
        let stopped_waiting = || ActionHostError::Unavailable {
            reason: "the caller stopped waiting for admission".into(),
        };
        let room = destination.workspace.room.to_string();
        let app_id = &destination.app_id;
        let address = &request.target.address;
        let timeout = self.launchable.delivery_timeout();
        let deadline = tokio::time::sleep(timeout);
        let abandoned = completer.abandoned();
        tokio::pin!(deadline, abandoned);
        // Taken before the first look, so a publication between that look
        // and the wait below still wakes it.
        let mut changes = self.publisher_changes();
        let mut launched = false;
        loop {
            self.check_launch_origin(caller, origin, &room)?;
            if completer.is_abandoned() {
                return Err(stopped_waiting());
            }
            if let Some(peer_id) =
                self.ready_publisher(app_id, &room, destination.record.as_deref(), address)
            {
                let mut delivered = request.clone();
                delivered.target.provider = peer_id.to_string();
                return self.start_for(caller, None, None, origin, &delivered);
            }
            if !launched {
                // Re-read the launch policy immediately before the actual host call.
                let current = self.admit_launchable(caller, request)?;
                if current.workspace.room != destination.workspace.room
                    || current.app_id != *app_id
                    || current.record != destination.record
                {
                    return Err(ActionHostError::NotPermitted);
                }
                let state = Arc::clone(self);
                let app = app_id.clone();
                tokio::task::spawn_blocking(move || {
                    state.launch_retained(&app, &current.workspace)
                })
                .await
                .unwrap_or_else(|error| Err(error.to_string()))
                .map_err(|reason| ActionHostError::Refused {
                    reason: format!("{app_id} could not be launched: {reason}"),
                })?;
                launched = true;
                continue;
            }
            tokio::select! {
                seen = changes.changed() => if seen.is_err() {
                    return Err(ActionHostError::Unavailable {
                        reason: "the broker stopped".into(),
                    });
                },
                () = &mut deadline => {
                    return Err(ActionHostError::Unavailable {
                        reason: format!(
                            "{app_id} was launched but did not offer \"{}\" within {:.1} s",
                            address.action,
                            timeout.as_secs_f32()
                        ),
                    });
                }
                () = &mut abandoned => return Err(stopped_waiting()),
            }
        }
    }

    /// Recheck the original signed custody while an asynchronous launch waits.
    /// The numeric dispatcher checks it again when capturing the actual offer.
    fn check_launch_origin(
        &self,
        caller: &Caller,
        origin: Option<&super::published_invocation::InvocationCaller>,
        room: &str,
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
                if original.room != room {
                    return Err(ActionHostError::NotPermitted);
                }
                Some(self.current_action_caller(original).map_err(refusal)?)
            }
            Some(super::published_invocation::InvocationCaller::Local { route, admitted }) => {
                if route.cluster_id != room {
                    return Err(ActionHostError::NotPermitted);
                }
                Some(
                    self.revalidate_admission(route, admitted)
                        .map_err(|error| refusal(error.to_string()))?,
                )
            }
            Some(super::published_invocation::InvocationCaller::NativePerson {
                room: original_room,
                certificate,
                ..
            }) => {
                if original_room != room
                    || caller.principal()
                        != &tilo_actions::remote::CallerPrincipal::Person(
                            super::principal_id_for_root(certificate.user_public_key),
                        )
                {
                    return Err(ActionHostError::NotPermitted);
                }
                self.current_native_person(room, certificate)
                    .map_err(refusal)?;
                None
            }
            None => None,
        };
        if let Some(admitted) = admitted {
            let expected = admitted.broker_actor();
            let actual = BrokerActor::for_caller(caller);
            if expected.id != actual.id || expected.is_agent != actual.is_agent {
                return Err(ActionHostError::NotPermitted);
            }
        }
        Ok(())
    }

    /// Start `app_id` in `workspace` through the host's launcher, or the one
    /// a test installed in its place.
    fn launch_retained(&self, app_id: &str, workspace: &Workspace) -> Result<(), String> {
        if let Some(launcher) = self.launchable.launcher() {
            return launcher(app_id, workspace);
        }
        self.services
            .launcher
            .as_ref()
            .ok_or_else(|| "application launch unsupported".to_string())?
            .launch(app_id, workspace)
    }

    /// The live connection of `app_id` in `room` that publishes `address`
    /// and can receive invocations.
    fn ready_publisher(
        &self,
        app_id: &str,
        room: &str,
        record: Option<&str>,
        address: &tilo_actions::ActionAddress,
    ) -> Option<u64> {
        let subscribers = self.invocation_subscribers.lock().ok()?;
        let surfaces = self.surfaces.lock().ok()?;
        surfaces
            .values()
            .filter(|surface| {
                surface.live
                    && surface.app_id == app_id
                    && surface.route.cluster_id == room
                    && record.is_none_or(|record| surface.route.record_id == record)
                    && subscribers.contains_key(&surface.peer_id)
                    && surface.actions.iter().any(|action| {
                        action.resource == address.resource && action.action == address.action
                    })
            })
            .map(|surface| surface.peer_id)
            .min()
    }

    fn invoke_eager(
        &self,
        caller: &Caller,
        own_peer: Option<u64>,
        launch: Option<&LaunchScope>,
        origin: Option<&super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        let mut ledger =
            self.remote_invocations
                .lock()
                .map_err(|_| ActionHostError::Unavailable {
                    reason: "the invocation ledger is unavailable".into(),
                })?;
        let (job, _) = ledger.admit(caller, request, || {
            self.start_for(caller, own_peer, launch, origin, request)
        })?;
        let job = *job;
        drop(ledger);
        Ok(self.receipt_for(request.invocation, job))
    }

    fn receipt_for(&self, invocation: tilo_actions::InvocationId, job: u64) -> InvocationReceipt {
        self.invocation_status(job).map_or(
            InvocationReceipt {
                invocation,
                status: ReceiptStatus::Accepted,
            },
            |update| InvocationState::new(invocation, update).receipt(),
        )
    }

    /// The directory publishing `request`'s target, when the action is not
    /// on the provider's eager surface.
    fn directory_target(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<ActionDirectoryRoot> {
        let peer_id = request.target.provider.parse::<u64>().ok()?;
        let address = &request.target.address;
        let eager = self.surfaces.lock().is_ok_and(|surfaces| {
            surfaces.get(&peer_id).is_some_and(|surface| {
                surface.live
                    && surface.actions.iter().any(|action| {
                        action.resource == address.resource && action.action == address.action
                    })
            })
        });
        (!eager)
            .then(|| self.directory_root_for(caller, peer_id))
            .flatten()
    }

    fn invoke_directory(
        self: &Arc<Self>,
        caller: &Caller,
        root: ActionDirectoryRoot,
        origin: Option<&super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let ValidatedDirectoryInvocation { mode, argument } =
            match validate_directory_invocation(&root, request) {
                Ok(validated) => validated,
                Err(error) => return Pending::ready(Err(error)),
            };
        let address = &request.target.address;
        let reservation = match self.remote_invocations.lock() {
            Ok(mut ledger) => ledger.reserve(caller, request),
            Err(_) => Err(ActionHostError::Unavailable {
                reason: "the invocation ledger is unavailable".into(),
            }),
        };
        match reservation {
            Err(error) => return Pending::ready(Err(error)),
            Ok(Reservation::Admitted) => {
                return Pending::ready(
                    self.remote_job(caller, &request.invocation)
                        .map(|job| self.receipt_for(request.invocation, job))
                        .ok_or(ActionHostError::UnknownInvocation),
                );
            }
            Ok(Reservation::Reserved) => {}
        }
        let actor = self.caller_actor(caller, &root.cluster_id);
        let queue = self.action_directory_request_queue();
        let resolved = match mode {
            DirectoryResolutionMode::Revision(revision) => queue.try_resolve_pending(
                actor,
                &root.cluster_id,
                root.peer_id,
                revision,
                &address.resource,
                &address.action,
            ),
            DirectoryResolutionMode::Fresh(binding) => queue.try_resolve_fresh_pending(
                actor,
                &root.cluster_id,
                root.peer_id,
                &address.resource,
                &address.action,
                runtime_fresh_binding(&binding),
            ),
        };
        let resolved = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                if let Ok(mut ledger) = self.remote_invocations.lock() {
                    ledger.abandon(&request.invocation);
                }
                return Pending::ready(Err(bridge_error(error)));
            }
        };
        let (completer, admitted) = pending();
        let state = Arc::clone(self);
        let caller = caller.clone();
        let request = request.clone();
        let origin = origin.cloned();
        // Runs on the broker's local set in the same turn the owner answered,
        // so nothing can change between resolution and admission.
        resolved.then(move |reply| {
            // A caller that stopped waiting (its admission deadline passed)
            // has already been told the action did not start; keep it so.
            let started = if completer.is_abandoned() {
                Err(ActionHostError::Unavailable {
                    reason: "the caller stopped waiting for admission".into(),
                })
            } else {
                state.start_resolved(
                    &caller,
                    &root,
                    &request,
                    origin.as_ref(),
                    argument.as_deref(),
                    reply,
                )
            };
            completer.complete(state.finish_directory_admission(request.invocation, started));
        });
        admitted
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

    fn start_resolved(
        &self,
        caller: &Caller,
        root: &ActionDirectoryRoot,
        request: &InvokeRequest,
        origin: Option<&super::published_invocation::InvocationCaller>,
        argument: Option<&str>,
        reply: Option<ActionDirectoryBridgeReply>,
    ) -> Result<u64, ActionHostError> {
        let resolved = match reply {
            Some(Ok(ActionDirectoryBridgeResponse::Resolution(resolved))) => resolved,
            Some(Ok(_)) => return Err(ActionHostError::UnknownAction),
            Some(Err(error)) => return Err(bridge_error(error)),
            None => {
                return Err(ActionHostError::Unavailable {
                    reason: "the action directory went away".into(),
                });
            }
        };
        let row = match resolved.resolution {
            runtime_collab::ActionDirectoryResolution::Found(row) => row,
            runtime_collab::ActionDirectoryResolution::StaleRevision => {
                return Err(ActionHostError::InvalidRequest {
                    reason: "the action changed since that revision; list it again".into(),
                });
            }
            _ => return Err(ActionHostError::UnknownAction),
        };
        if let Some(expected) = &request.fresh_binding {
            let current =
                directory_fresh_binding(&row).map_err(|_| ActionHostError::UnknownAction)?;
            if resolved.revision.is_empty() || current.as_ref() != Some(expected) {
                return Err(ActionHostError::UnknownAction);
            }
        }
        let offered =
            published_directory_action(&row).map_err(|_| ActionHostError::UnknownAction)?;
        let source_revision = request.revision.as_deref().or_else(|| {
            request
                .fresh_binding
                .as_ref()
                .map(|_| resolved.revision.as_str())
        });
        let actor = self.caller_actor(caller, &root.cluster_id);
        let token = self.confirmation_token(
            root.peer_id,
            &offered,
            argument,
            &actor.id,
            request.confirmation.as_ref(),
        )?;
        let surface = PublishedSurface {
            peer_id: root.peer_id,
            app_id: root.app_id.clone(),
            route: CrdtRoute {
                app_id: root.app_id.clone(),
                cluster_id: root.cluster_id.clone(),
                record_id: root.record_id.clone(),
            },
            actions: vec![offered.clone()],
            live: true,
        };
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| ActionHostError::Unavailable {
                reason: "invocation admission unavailable".into(),
            })?;
        if request
            .target
            .provider
            .starts_with(super::canonical_route::PROVIDER_PREFIX)
            && self.canonical_token_directory(&request.target.provider)? != *root
        {
            return Err(ActionHostError::UnknownAction);
        }
        self.dispatch_invocation_from_offered_owned(
            root.peer_id,
            (surface, offered),
            InvocationInputs {
                caller: Some(caller),
                requested: Some(request.invocation),
                execution: super::ExecutionScope::for_call(caller, request.invocation),
                managed_interaction: None,
                argument,
                actor: &actor,
                confirmation_token: token.as_deref(),
                context: request.context.clone(),
            },
            super::AdmissionProvenance {
                durable: None,
                revision: source_revision,
                fresh_binding: request.fresh_binding.as_ref(),
                origin: origin.cloned(),
                retained_time: None,
            },
        )
        .map(|job| job.id)
        .map_err(refusal)
    }

    fn start_for(
        &self,
        caller: &Caller,
        own_peer: Option<u64>,
        launch: Option<&LaunchScope>,
        origin: Option<&super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
    ) -> Result<u64, ActionHostError> {
        let address = &request.target.address;
        let argument = request.argument.to_slot();
        let provider = request.target.provider.as_str();
        let peer_id = if let Some(room) = workspace_provider_room(provider) {
            let task_peer = self
                .workspace_task_peers
                .lock()
                .ok()
                .and_then(|peers| peers.get(room).copied())
                .filter(|peer| {
                    self.published_surfaces().into_iter().any(|surface| {
                        surface.peer_id == *peer
                            && surface.live
                            && surface.route.cluster_id == room
                            && surface.actions.iter().any(|action| {
                                action.resource == address.resource
                                    && action.action == address.action
                            })
                    })
                });
            let Some(peer) = task_peer else {
                return self.start_workspace_action(
                    caller,
                    &self.caller_actor(caller, room),
                    room,
                    launch,
                    origin,
                    request,
                    argument.as_deref(),
                );
            };
            peer
        } else if provider == OWN_PROVIDER {
            own_peer.ok_or(ActionHostError::UnknownAction)?
        } else {
            provider
                .parse::<u64>()
                .map_err(|_| ActionHostError::UnknownAction)?
        };
        // Only the one action this peer offers is copied out, not every
        // app's published surface.
        let (app_id, room, action) = self
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
        let (offered, room) = self
            .workspaces_for(caller)
            .into_iter()
            .find(|(workspace, _, _)| workspace.room.to_string() == room)
            .and_then(|(workspace, policy, now)| {
                Self::allowed(
                    &workspace,
                    &policy,
                    now,
                    &app_id,
                    &action.resource,
                    &action.action,
                    action.contract.permission,
                )
                .then_some((action, room))
            })
            .ok_or(ActionHostError::UnknownAction)?;
        let actor = self.caller_actor(caller, &room);
        let token = self.confirmation_token(
            peer_id,
            &offered,
            argument.as_deref(),
            &actor.id,
            request.confirmation.as_ref(),
        )?;
        self.start_invocation_confirmed_for_origin(
            peer_id,
            &address.resource,
            &address.action,
            InvocationInputs {
                caller: Some(caller),
                requested: Some(request.invocation),
                execution: super::ExecutionScope::for_call(caller, request.invocation),
                managed_interaction: None,
                argument: argument.as_deref(),
                actor: &actor,
                confirmation_token: token.as_deref(),
                context: request.context.clone(),
            },
            origin.cloned(),
        )
        .map(|job| job.id)
        .map_err(refusal)
    }

    fn start_workspace_action(
        &self,
        caller: &Caller,
        actor: &BrokerActor,
        room: &str,
        launch: Option<&LaunchScope>,
        origin: Option<&super::published_invocation::InvocationCaller>,
        request: &InvokeRequest,
        argument: Option<&str>,
    ) -> Result<u64, ActionHostError> {
        let address = &request.target.address;
        let Some((workspace, policy, now)) = self
            .workspaces_for(caller)
            .into_iter()
            .find(|(workspace, _, _)| workspace.room.to_string() == room)
        else {
            return Err(ActionHostError::UnknownAction);
        };
        if let Some(app_id) = address.resource.strip_prefix(APPLICATION_RESOURCE_PREFIX)
            && address.action == APPLICATION_OPEN_ACTION
        {
            if caller.room().is_none() {
                return Err(ActionHostError::UnknownAction);
            }
            let author = match caller.principal() {
                CallerPrincipal::Person(person)
                    if workspace.decide_at(&policy, &ResourceRef::Workspace, Verb::Write, now)
                        == CapDecision::Allow =>
                {
                    Some(person.as_str())
                }
                _ => None,
            };
            return self
                .launch_application(
                    &workspace,
                    &policy,
                    now,
                    actor,
                    app_id,
                    crate::services::LaunchContent {
                        hash: launch_content_hash(argument),
                        caller: launch,
                        author,
                    },
                )
                .map_err(refusal);
        }
        if let Some(app_id) = address.resource.strip_prefix(APPLICATION_RESOURCE_PREFIX)
            && address.action == APPLICATION_CLOSE_ACTION
        {
            if caller.room().is_none() {
                return Err(ActionHostError::UnknownAction);
            }
            return self
                .close_application(&workspace, &policy, now, actor, app_id)
                .map_err(refusal);
        }
        if address.resource == automation::LIBRARY_RESOURCE
            && self.services.shortcut_library.is_some()
        {
            return self.start_shortcut_library(
                caller,
                actor,
                (&workspace, &policy, now),
                &address.action,
                argument,
            );
        }
        if let Some(service) = self.services.keyboard_shortcuts.as_ref()
            && service.is_resource(&address.resource)
            && caller.device_public_key().is_none()
        {
            return self.start_keyboard_shortcut_action(
                service.as_ref(),
                actor,
                (&workspace, &policy, now),
                address,
                argument,
            );
        }
        if let Some(governance) = self.services.workspace_governance.as_ref()
            && governance.handles(&address.action)
        {
            let certificate = self
                .governance_person_certificate(caller, room, origin)
                .map_err(refusal)?;
            return self
                .invoke_workspace_governance_action(room, actor, request, argument, certificate)
                .map_err(refusal);
        }
        Err(ActionHostError::UnknownAction)
    }

    /// Apply one shortcut library action for `caller` in `workspace`. A
    /// saved shortcut is checked against exactly what `caller` is offered now.
    fn start_shortcut_library(
        &self,
        caller: &Caller,
        actor: &BrokerActor,
        (workspace, policy, now): (&Workspace, &Actor, u64),
        action: &str,
        argument: Option<&str>,
    ) -> Result<u64, ActionHostError> {
        let library = self
            .services
            .shortcut_library
            .as_ref()
            .ok_or(ActionHostError::UnknownAction)?;
        let descriptor =
            automation::library_descriptor(action).ok_or(ActionHostError::UnknownAction)?;
        if !Self::allowed(
            workspace,
            policy,
            now,
            SHORTCUTS_APP_ID,
            &descriptor.address.resource,
            action,
            descriptor.contract.permission,
        ) {
            return Err(ActionHostError::NotPermitted);
        }
        let operation = automation::decode_library(action, argument)
            .map_err(|reason| ActionHostError::InvalidRequest { reason })?;
        let catalog = tilo_automation::ActionCatalog::new(self.eager_catalogue(caller, true).0);
        let keymap = library.keymap();
        let room = workspace.room.to_string();
        let plan = automation::plan_library(
            workspace,
            operation,
            &automation::SaveContext {
                catalog: &catalog,
                saver: &actor.id,
                keymap: &keymap,
                fixed_label: &|fixed| library.fixed_shortcut_label(fixed),
            },
        )
        .map_err(|reason| ActionHostError::Refused { reason })?;
        let id = self
            .invocation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let output = match plan {
            automation::LibraryPlan::Answer(output) => output,
            automation::LibraryPlan::Change { change, output } => {
                library
                    .store(&crate::services::ShortcutLibraryRequest {
                        room: &room,
                        actor,
                        change: &change,
                        context: automation::library_context(&descriptor),
                        invocation,
                    })
                    .map_err(|reason| ActionHostError::Refused { reason })?;
                output
            }
        };
        self.record_completion(
            id,
            InvocationOwner::Local {
                actor: actor.id.clone(),
            },
            tilo_actions::InvocationUpdate::Succeeded { invocation, output },
        );
        Ok(id)
    }

    /// List or change the desktop's keyboard shortcuts for `actor` in
    /// `workspace`. Changing one takes the same Settings grant as changing
    /// any other setting; a clash fails naming what holds the chord.
    fn start_keyboard_shortcut_action(
        &self,
        service: &dyn crate::services::KeyboardShortcutServices,
        actor: &BrokerActor,
        (workspace, policy, now): (&Workspace, &Actor, u64),
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Result<u64, ActionHostError> {
        let mut keymap = settings_store::hardware::ShortcutConfig::load();
        let descriptor = service
            .descriptor(&keymap, address)
            .ok_or(ActionHostError::UnknownAction)?;
        if !Self::allowed(
            workspace,
            policy,
            now,
            service.app_id(),
            &address.resource,
            &address.action,
            descriptor.contract.permission,
        ) {
            return Err(ActionHostError::NotPermitted);
        }
        let id = self
            .invocation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let outcome =
            service
                .apply(&mut keymap, address, argument)
                .and_then(|(output, changed)| {
                    if changed {
                        keymap.save().map_err(|error| {
                            Box::new(tilo_actions::ActionFailure {
                                code: "save_failed".into(),
                                message: error.to_string(),
                                details: None,
                                retryable: true,
                            })
                        })?;
                    }
                    Ok(output)
                });
        self.record_completion(
            id,
            InvocationOwner::Local {
                actor: actor.id.clone(),
            },
            match outcome {
                Ok(output) => tilo_actions::InvocationUpdate::Succeeded { invocation, output },
                Err(error) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: *error,
                },
            },
        );
        Ok(id)
    }

    /// Admit a run of saved shortcut `shortcut_id` in `room` for `caller`
    /// and start it. The run invokes every step through this broker as
    /// `caller`, so each step meets `caller`'s own authority as well as the
    /// shortcut's grant; the admission answers at once and the run reports
    /// its outcome on the invocation when it ends. The run's id is the
    /// invocation's, so the history it leaves is found from the receipt.
    fn invoke_shortcut_run(
        self: &Arc<Self>,
        caller: &Caller,
        room: &str,
        shortcut_id: &str,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        let abandon = |error: ActionHostError| {
            if let Ok(mut ledger) = self.remote_invocations.lock() {
                ledger.abandon(&request.invocation);
            }
            error
        };
        let reservation = self
            .remote_invocations
            .lock()
            .map_err(|_| ActionHostError::Unavailable {
                reason: "the invocation ledger is unavailable".into(),
            })?
            .reserve(caller, request)?;
        if matches!(reservation, Reservation::Admitted) {
            return self
                .remote_job(caller, &request.invocation)
                .map(|job| self.receipt_for(request.invocation, job))
                .ok_or(ActionHostError::UnknownInvocation);
        }
        let Some(shortcut) = self.saved_shortcut(caller, room, shortcut_id) else {
            return Err(abandon(ActionHostError::UnknownAction));
        };
        let inputs = match &request.argument {
            tilo_actions::remote::InvokeArgument::None => serde_json::Map::new(),
            tilo_actions::remote::InvokeArgument::Json(inputs) => inputs.clone(),
            tilo_actions::remote::InvokeArgument::Text(_) => {
                return Err(abandon(ActionHostError::InvalidRequest {
                    reason: "a shortcut takes its inputs as JSON arguments".into(),
                }));
            }
        };
        let job = self
            .start_shortcut_run(
                caller,
                room,
                shortcut,
                automation::RunStart {
                    run: tilo_automation::RunId(request.invocation.0),
                    trigger: tilo_automation::RunTrigger::Manual,
                    mode: automation::run_mode(request.confirmation.as_ref()),
                    inputs,
                },
                |runtime| automation::DoorHooks::new(request.confirmation.clone(), runtime),
            )
            .map_err(abandon)?;
        if let Ok(mut ledger) = self.remote_invocations.lock() {
            ledger.complete(&request.invocation, job);
        }
        Ok(self.receipt_for(request.invocation, job))
    }

    /// Run saved shortcut `shortcut_id` in `room` for `caller`, who is at
    /// the screen: every step that needs confirmation and every question
    /// waits in [`Self::take_step_prompt`] until the compositor's sheet
    /// answers it.
    ///
    /// # Errors
    ///
    /// Why the shortcut cannot run for `caller`.
    pub(crate) fn start_interactive_shortcut_run(
        self: &Arc<Self>,
        caller: &Caller,
        room: &str,
        shortcut_id: &str,
        inputs: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, ActionHostError> {
        let shortcut = self
            .saved_shortcut(caller, room, shortcut_id)
            .ok_or(ActionHostError::UnknownAction)?;
        let prompts = self.step_prompts.clone();
        let name = shortcut.name.clone();
        let hooks = |runtime| automation::SheetHooks::new(prompts, name, runtime);
        self.start_shortcut_run(
            caller,
            room,
            shortcut,
            automation::RunStart {
                run: tilo_automation::RunId::new(),
                trigger: tilo_automation::RunTrigger::Manual,
                mode: tilo_automation::RunMode::Interactive,
                inputs,
            },
            hooks,
        )
    }

    /// The oldest step of an interactive run waiting for its person.
    pub(crate) fn take_step_prompt(&self) -> Option<automation::PromptedStep> {
        self.step_prompts.take_next()
    }

    /// Start one of saved shortcut `shortcut_id`'s triggers, `trigger`, in
    /// `room` on behalf of `caller`, who must be the person who saved it.
    ///
    /// A keyboard trigger means the person is at the screen: its run asks
    /// them through the compositor's sheet, like a palette run. Nobody is
    /// present at a scheduled run: a step asking for confirmation proceeds
    /// only when its saver consented to exactly that action in advance, and
    /// a question stops the run. Before anything runs the
    /// shortcut is checked again against what `caller` is offered now, so a
    /// shortcut that was deleted, lost the trigger, or reaches past a grant
    /// that has since been revoked does not start.
    ///
    /// # Errors
    ///
    /// Why the trigger may not start the shortcut.
    pub fn start_triggered_shortcut_run(
        self: &Arc<Self>,
        caller: &Caller,
        room: &str,
        shortcut_id: &str,
        trigger: &tilo_automation::TriggerSpec,
        run: tilo_automation::RunId,
        inputs: serde_json::Map<String, serde_json::Value>,
    ) -> Result<u64, ActionHostError> {
        let shortcut = self
            .saved_shortcut(caller, room, shortcut_id)
            .ok_or(ActionHostError::UnknownAction)?;
        let actor = BrokerActor::for_caller(caller).id;
        if shortcut.saved_by.as_deref() != Some(actor.as_str()) {
            return Err(ActionHostError::NotPermitted);
        }
        if !shortcut.triggers.contains(trigger) {
            return Err(ActionHostError::Refused {
                reason: "the shortcut no longer has that trigger".into(),
            });
        }
        let catalog = tilo_automation::ActionCatalog::new(self.eager_catalogue(caller, true).0);
        let others = self
            .workspaces_for(caller)
            .into_iter()
            .find(|(workspace, _, _)| workspace.room.to_string() == room)
            .map(|(workspace, _, _)| workspace.shortcuts)
            .unwrap_or_default();
        let resolver = |id: &str| others.iter().find(|other| other.id == id).cloned();
        tilo_automation::check(&shortcut, &catalog, &resolver).map_err(|errors| {
            ActionHostError::Refused {
                reason: errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; "),
            }
        })?;
        if matches!(trigger, tilo_automation::TriggerSpec::Keyboard { .. }) {
            let prompts = self.step_prompts.clone();
            let name = shortcut.name.clone();
            let hooks = |runtime| automation::SheetHooks::new(prompts, name, runtime);
            return self.start_shortcut_run(
                caller,
                room,
                shortcut,
                automation::RunStart {
                    run,
                    trigger: tilo_automation::RunTrigger::Fired(trigger.clone()),
                    mode: tilo_automation::RunMode::Interactive,
                    inputs: serde_json::Map::new(),
                },
                hooks,
            );
        }
        self.start_shortcut_run(
            caller,
            room,
            shortcut,
            automation::RunStart {
                run,
                trigger: tilo_automation::RunTrigger::Fired(trigger.clone()),
                mode: tilo_automation::RunMode::Unattended,
                inputs,
            },
            |runtime| automation::DoorHooks::new(None, runtime),
        )
    }

    /// Shortcut `shortcut_id` as saved in `room`, when `caller` is offered
    /// that workspace.
    fn saved_shortcut(
        &self,
        caller: &Caller,
        room: &str,
        shortcut_id: &str,
    ) -> Option<tilo_automation::Shortcut> {
        self.workspaces_for(caller)
            .into_iter()
            .find(|(workspace, _, _)| workspace.room.to_string() == room)
            .and_then(|(workspace, _, _)| {
                workspace
                    .shortcuts
                    .into_iter()
                    .find(|shortcut| shortcut.id == shortcut_id)
            })
    }

    /// Run `shortcut` as `caller` through this broker, reporting on a new
    /// broker job whose id is returned at once. The saver's advance consents
    /// go only to a run the saver started.
    fn start_shortcut_run<K>(
        self: &Arc<Self>,
        caller: &Caller,
        room: &str,
        shortcut: tilo_automation::Shortcut,
        start: automation::RunStart,
        hooks: impl FnOnce(tokio::runtime::Handle) -> K,
    ) -> Result<u64, ActionHostError>
    where
        K: tilo_automation::RunHooks + Send + 'static,
    {
        let runtime = self
            .services
            .shortcut_library
            .as_ref()
            .ok_or(ActionHostError::UnknownAction)?
            .runtime()
            .map_err(|reason| ActionHostError::Unavailable { reason })?;
        let hooks = hooks(runtime.clone());
        let actor = BrokerActor::for_caller(caller).id;
        let job = self
            .invocation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(job)));
        self.record_completion(
            job,
            InvocationOwner::Local {
                actor: actor.clone(),
            },
            tilo_actions::InvocationUpdate::Accepted { invocation },
        );
        let host = BrokerDoor::new(Arc::clone(self));
        let consents = if shortcut.saved_by.as_deref() == Some(actor.as_str()) {
            shortcut.consents.clone()
        } else {
            Vec::new()
        };
        let state = Arc::clone(self);
        let caller = caller.clone();
        let room = room.to_string();
        runtime.spawn(async move {
            let report = tilo_automation::run(
                &host,
                &hooks,
                tilo_automation::RunRequest {
                    shortcut: &shortcut,
                    run: start.run,
                    caller,
                    mode: start.mode,
                    trigger: start.trigger,
                    inputs: start.inputs,
                    consents,
                },
            )
            .await;
            state.record_completion(
                job,
                InvocationOwner::Local {
                    actor: actor.clone(),
                },
                automation::completion(invocation, &report),
            );
            state.shortcut_runs.push(automation::FinishedShortcutRun {
                room,
                shortcut,
                invoker: actor,
                report,
            });
        });
        Ok(job)
    }

    /// Shortcut runs finished since the last call, oldest first.
    pub fn take_finished_shortcut_runs(&self) -> Vec<automation::FinishedShortcutRun> {
        self.shortcut_runs.take()
    }

    /// Whether a finished shortcut run is waiting to be taken.
    pub fn has_finished_shortcut_runs(&self) -> bool {
        self.shortcut_runs.has_finished()
    }

    /// Execute a host-owned workspace governance action (permission
    /// profiles). There is no application to forward to: the host's
    /// governance service validates, applies and records it under its own
    /// capability authority, and the broker reports one terminal update.
    fn invoke_workspace_governance_action(
        &self,
        room: &str,
        actor: &BrokerActor,
        request: &InvokeRequest,
        argument: Option<&str>,
        certificate: Option<tilo_user_identity::DeviceCert>,
    ) -> Result<u64, String> {
        let governance = self
            .services
            .workspace_governance
            .as_ref()
            .ok_or_else(|| "workspace governance is unavailable on this host".to_string())?;
        let id = self
            .invocation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let invocation = request.invocation;
        governance.invoke(&crate::services::WorkspaceGovernanceRequest {
            room,
            actor,
            resource: &request.target.address.resource,
            action: &request.target.address.action,
            argument,
            confirmation: &confirmation_evidence(request.confirmation.as_ref()),
            invocation,
            certificate,
        })?;
        self.record_completion(
            id,
            InvocationOwner::Local {
                actor: actor.id.clone(),
            },
            tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
        );
        Ok(id)
    }

    fn governance_person_certificate(
        &self,
        caller: &Caller,
        room: &str,
        origin: Option<&super::published_invocation::InvocationCaller>,
    ) -> Result<Option<tilo_user_identity::DeviceCert>, String> {
        use super::published_invocation::InvocationCaller;
        let admitted = match origin {
            Some(InvocationCaller::NativePerson {
                room: original_room,
                certificate,
                ..
            }) => {
                if original_room != room
                    || caller.room() != Some(room)
                    || caller.principal()
                        != &CallerPrincipal::Person(super::principal_id_for_root(
                            certificate.user_public_key,
                        ))
                    || caller
                        .user_public_key()
                        .is_some_and(|key| key != &certificate.user_public_key)
                    || caller
                        .device_public_key()
                        .is_some_and(|key| key != &certificate.device_public_key)
                    || certificate.capabilities.can_act_as_host
                    || super::cap_applies_in(
                        self.certificate_authority.get().map(AsRef::as_ref),
                        room,
                        caller.is_foreign_device() || certificate.capabilities.foreign,
                        Some(&certificate.device_public_key),
                    )
                {
                    return Err("governance requires original Person evidence".into());
                }
                self.current_native_person(original_room, certificate)?;
                return Ok(Some(certificate.clone()));
            }
            Some(InvocationCaller::Certified { caller, .. }) => {
                if caller.room != room {
                    return Err("governance original room changed".into());
                }
                self.current_action_caller(caller)?
            }
            Some(InvocationCaller::Local { route, admitted }) => {
                if route.cluster_id != room {
                    return Err("governance original room changed".into());
                }
                self.revalidate_admission(route, admitted)
                    .map_err(|_| "governance original admission ended")?;
                *admitted.clone()
            }
            None => return Ok(None),
        };
        let certificate = admitted
            .certificate
            .ok_or("governance original certificate absent")?;
        let restricted = caller.is_foreign_device()
            || admitted.verified.capped
            || matches!(
                origin,
                Some(InvocationCaller::Certified {
                    foreign_restriction: true,
                    ..
                })
            );
        if admitted.agent_id.is_some()
            || certificate.capabilities.can_act_as_host
            || !matches!(caller.principal(), CallerPrincipal::Person(_))
            || caller.user_public_key() != Some(&certificate.user_public_key)
            || caller.device_public_key() != Some(&certificate.device_public_key)
            || super::cap_applies_in(
                self.certificate_authority.get().map(AsRef::as_ref),
                room,
                restricted,
                Some(&certificate.device_public_key),
            )
        {
            return Err("governance requires original Person evidence".into());
        }
        Ok(Some(certificate))
    }

    /// Launch `app_id` into `workspace`, optionally opening content-addressed
    /// bytes read-only, and record the terminal outcome as an invocation.
    ///
    /// # Errors
    ///
    /// Returns a message when the caller may not launch the application or no launcher is installed.
    pub fn launch_application(
        &self,
        workspace: &Workspace,
        policy: &Actor,
        now: u64,
        actor: &BrokerActor,
        app_id: &str,
        content: crate::services::LaunchContent<'_>,
    ) -> Result<u64, String> {
        if workspace.decide_at(
            policy,
            &ResourceRef::App(app_id.to_string()),
            Verb::Write,
            now,
        ) != CapDecision::Allow
        {
            return Err("the caller is not allowed to launch that application".into());
        }
        // Content is only ever read through the asking application's own
        // scope; a caller without one cannot open it by hash.
        if !content.hash.is_empty() && content.caller.is_none() {
            return Err("launch content requires an application the launcher authenticated".into());
        }
        let launcher = self
            .services
            .launcher
            .as_ref()
            .ok_or_else(|| "application launch unsupported".to_string())?;
        let id = self
            .invocation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let update = match launcher.launch_content(app_id, workspace, content) {
            Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            Err(error) => tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "launch_failed".into(),
                    message: error,
                    details: None,
                    retryable: false,
                },
            },
        };
        self.record_completion(
            id,
            InvocationOwner::Local {
                actor: actor.id.clone(),
            },
            update,
        );
        Ok(id)
    }

    /// Close `app_id`'s windows in `workspace` and record the terminal
    /// outcome as an invocation.
    ///
    /// # Errors
    ///
    /// Returns a message when the caller may not change the application or
    /// the host cannot close applications.
    pub fn close_application(
        &self,
        workspace: &Workspace,
        policy: &Actor,
        now: u64,
        actor: &BrokerActor,
        app_id: &str,
    ) -> Result<u64, String> {
        if workspace.decide_at(
            policy,
            &ResourceRef::App(app_id.to_string()),
            Verb::Write,
            now,
        ) != CapDecision::Allow
        {
            return Err("the caller is not allowed to close that application".into());
        }
        let launcher = self
            .services
            .launcher
            .as_ref()
            .filter(|launcher| launcher.closes_applications())
            .ok_or_else(|| "closing applications is unsupported on this host".to_string())?;
        let id = self
            .invocation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let update = match launcher.close(app_id, workspace) {
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
        self.record_completion(
            id,
            InvocationOwner::Local {
                actor: actor.id.clone(),
            },
            update,
        );
        Ok(id)
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
            .map(|update| InvocationState::new(*id, update))
    }

    /// Observe a numeric job only when it is still bound to this exact
    /// certified caller and its retained invocation UUID.
    pub(crate) fn action_observe_job(
        &self,
        caller: &Caller,
        job: u64,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if check_execution(caller).is_err() {
            report_job_binding_refusal("execution_ended");
            return None;
        }
        if caller.user_public_key().is_none() || caller.device_public_key().is_none() {
            report_job_binding_refusal("unkeyed_caller");
            return None;
        }
        let Some(update) = self.invocation_status(job) else {
            report_job_binding_refusal("job_absent");
            return None;
        };
        let Ok(ledger) = self.remote_invocations.lock() else {
            report_job_binding_refusal("ledger_unavailable");
            return None;
        };
        let retained_job = ledger
            .get_for_certified_device(caller, &update.invocation())
            .copied();
        if retained_job == Some(job) {
            return Some(update);
        }
        if !ledger.contains_admitted(&update.invocation()) {
            report_job_binding_refusal("invocation_not_retained");
        } else if retained_job.is_none() {
            report_job_binding_refusal("caller_binding_mismatch");
        } else {
            report_job_binding_refusal("job_handle_mismatch");
        }
        None
    }

    /// Whether `job` is retained as a remotely admitted invocation. Local UI
    /// projections use this only to suppress their weaker actor-name fallback.
    /// `None` means the authority ledger is unavailable and must fail closed.
    #[must_use]
    pub(crate) fn action_job_is_remote(&self, job: u64) -> Option<bool> {
        let Some(update) = self.invocation_status(job) else {
            return Some(false);
        };
        let ledger = self.remote_invocations.lock().ok()?;
        Some(ledger.contains_admitted(&update.invocation()))
    }

    #[must_use]
    pub(crate) fn action_can_cancel_job(&self, caller: &Caller, job: u64) -> bool {
        if self.action_observe_job(caller, job).is_none() {
            return false;
        }
        self.can_cancel_admitted_job(job)
    }

    /// Cancel `job` only when its immutable invocation remains associated with
    /// this exact certified Person/device and room.
    ///
    /// # Errors
    ///
    /// Returns [`ActionHostError::UnknownInvocation`] for a foreign/stale job,
    /// or [`ActionHostError::NotCancellable`] when its contract forbids it.
    pub(crate) fn action_cancel_job(
        &self,
        caller: &Caller,
        job: u64,
    ) -> Result<(), ActionHostError> {
        self.action_observe_job(caller, job)
            .ok_or(ActionHostError::UnknownInvocation)?;
        self.request_cancellation_for_admitted_job(job)
            .map_err(|reason| ActionHostError::NotCancellable { reason })
    }

    /// Resolves once an invocation changes after what `seen` has seen, or
    /// after [`CHANGE_SAFETY_INTERVAL`] without one — the
    /// [`ActionHost::next_change`](tilo_actions::remote::ActionHost::next_change)
    /// of the broker's hosts. `None` for an invocation `caller` did not
    /// admit, or off a runtime, where the waiter polls on its own schedule.
    pub(super) fn action_next_change(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
        mut seen: tokio::sync::watch::Receiver<u64>,
    ) -> Option<Pending<()>> {
        self.remote_job(caller, id)?;
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        let (changed, change) = pending();
        runtime.spawn(async move {
            tokio::select! {
                _ = seen.changed() => {}
                () = tilo_rt::sleep(CHANGE_SAFETY_INTERVAL) => {}
                // A waiter that found its answer some other way (it observed
                // the invocation finished) holds no task for the interval.
                () = changed.abandoned() => return,
            }
            changed.complete(());
        });
        Some(change)
    }

    /// [`Self::action_next_change`] counting only changes after this call:
    /// the `next_change` of a host whose waiter asks before it observes, so a
    /// change between the two still wakes it. Authorized exactly as
    /// [`Self::action_observe`]: `None` for an invocation `caller` did not
    /// admit.
    pub fn action_next_change_after_now(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<Pending<()>> {
        self.action_next_change(caller, id, self.invocation_changes())
    }

    /// Ask an invocation `caller` admitted to stop.
    ///
    /// # Errors
    ///
    /// Returns an error when `caller` admitted no such invocation or it cannot
    /// be cancelled.
    pub fn action_cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Result<(), ActionHostError> {
        let job = self
            .remote_job(caller, id)
            .ok_or(ActionHostError::UnknownInvocation)?;
        self.request_cancellation_for_admitted_job(job)
            .map_err(|reason| ActionHostError::NotCancellable { reason })
    }
}

fn action_row(provider: String, app_id: &str, action: ActionDescriptor) -> CatalogueRow {
    CatalogueRow {
        provider,
        app_id: app_id.to_string(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: action.address.resource.clone(),
            kind: action.resource_kind.clone(),
            label: action.resource_label.clone(),
            presentation: None,
        },
        action: Some(action),
        fresh_binding: None,
        has_children: false,
        visibility: None,
    }
}

/// A directory row, already validated and permission-checked on the local
/// set, as a catalogue row.
fn directory_row(
    root: &ActionDirectoryRoot,
    row: &runtime_collab::ActionDirectoryRow,
) -> Result<CatalogueRow, ActionHostError> {
    let fresh_binding =
        directory_fresh_binding(row).map_err(|_| ActionHostError::InvalidRequest {
            reason: "the action directory returned a malformed fresh binding".into(),
        })?;
    let action = if row.action.is_some() {
        Some(if let Some(binding) = &fresh_binding {
            binding.descriptor.clone()
        } else {
            descriptor(
                &published_directory_action(row).map_err(|_| ActionHostError::UnknownAction)?,
            )
        })
    } else {
        None
    };
    Ok(CatalogueRow {
        provider: root.peer_id.to_string(),
        app_id: root.app_id.clone(),
        parent_resource: row.parent_resource.clone(),
        resource: CatalogueResource {
            id: row.resource.clone(),
            kind: row.resource_kind.clone(),
            label: row.resource_label.clone(),
            presentation: row.presentation.clone(),
        },
        action,
        fresh_binding,
        has_children: row.has_children,
        visibility: row.visibility.clone(),
    })
}

/// Convert the owner's complete current binding without reconstructing the
/// execution locus that legacy directory rows do not publish.
///
/// Absence is the legitimate revision-bound form. Once an owner places a
/// fresh binding on the wire, every part must be exact and bounded; malformed
/// presence is refused rather than silently losing the caller's freshness
/// requirement.
pub(super) fn directory_fresh_binding(
    row: &runtime_collab::ActionDirectoryRow,
) -> Result<Option<FreshActionBinding>, capnp::Error> {
    let Some(binding) = row.fresh_binding.as_ref() else {
        return Ok(None);
    };
    let malformed = || capnp::Error::failed("malformed fresh action directory binding".into());
    if row.descriptor.as_ref() != Some(&binding.descriptor) {
        return Err(malformed());
    }
    if let Some(operation_type) = &binding.operation_type {
        super::validate_directory_identifier(
            operation_type,
            "fresh operation type",
            super::DIRECTORY_MAX_IDENTIFIER_BYTES,
            true,
        )?;
    }
    let offered = published_directory_action(row).map_err(|_| malformed())?;
    let locus = serde_json::from_str::<ExecutionLocus>(&binding.locus).map_err(|_| malformed())?;
    let mut descriptor = descriptor(&offered);
    descriptor.locus = locus;
    if descriptor.semantic_identity.is_none()
        || descriptor.address.resource != row.resource
        || descriptor.address.action != row.action.as_deref().ok_or_else(malformed)?
    {
        return Err(malformed());
    }
    Ok(Some(FreshActionBinding {
        descriptor,
        operation_type: binding.operation_type.clone(),
    }))
}

fn directory_page(
    root: &ActionDirectoryRoot,
    reply: ActionDirectoryBridgeReply,
    resolution_revision: Option<&str>,
) -> Result<CataloguePage, ActionHostError> {
    match reply.map_err(bridge_error)? {
        ActionDirectoryBridgeResponse::Page(page) => Ok(CataloguePage {
            locator: None,
            rows: page
                .rows
                .iter()
                .map(|row| directory_row(root, row))
                .collect::<Result<Vec<_>, _>>()?,
            revision: page.revision,
            providers: Vec::new(),
            next_cursor: page.next_cursor,
            exhausted: page.exhausted,
            stale_cursor: page.stale_cursor,
            budget_exhausted: page.budget_exhausted,
        }),
        ActionDirectoryBridgeResponse::Resolution(reply) => {
            let mut page = CataloguePage {
                exhausted: true,
                ..CataloguePage::default()
            };
            match reply.resolution {
                runtime_collab::ActionDirectoryResolution::Found(row) => {
                    let revision = resolution_revision.or_else(|| {
                        (!reply.revision.is_empty()).then_some(reply.revision.as_str())
                    });
                    let Some(revision) = revision else {
                        return Err(ActionHostError::Unavailable {
                            reason: "directory resolution lost its requested revision".into(),
                        });
                    };
                    revision.clone_into(&mut page.revision);
                    page.rows.push(directory_row(root, &row)?);
                }
                runtime_collab::ActionDirectoryResolution::StaleRevision => {
                    page.stale_cursor = true;
                }
                runtime_collab::ActionDirectoryResolution::NotFound
                | runtime_collab::ActionDirectoryResolution::Unavailable => {}
            }
            Ok(page)
        }
        // Only a start request answers with a started action; a query or
        // resolution never does.
        ActionDirectoryBridgeResponse::StartedDirectoryAction(_) => {
            Err(ActionHostError::Unavailable {
                reason: "the action directory answered a different request".into(),
            })
        }
    }
}

fn bridge_error(error: ActionDirectoryBridgeError) -> ActionHostError {
    match error {
        ActionDirectoryBridgeError::InvalidRequest => ActionHostError::InvalidRequest {
            reason: "the directory request is malformed".into(),
        },
        ActionDirectoryBridgeError::TimedOut => ActionHostError::Unavailable {
            reason: "the app has not answered its action directory for a long time: it is hung"
                .into(),
        },
        other => ActionHostError::Unavailable {
            reason: format!("the action directory is {other:?}"),
        },
    }
}

#[cfg(test)]
mod workspace_task_provider_tests {
    use super::super::{BrokerChannels, CapnpBrokerState, FanoutState, PeerPublisher};
    use super::{ActionHostError, FreshActionBinding};
    use crate::services::{HostServices, WorkspaceTaskSnapshot, WorkspaceTasks};
    use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
    use std::sync::{Arc, Mutex};

    struct ReadOnlyTasks(WorkspaceTaskSnapshot);

    impl WorkspaceTasks for ReadOnlyTasks {
        fn requires_room(&self, room: &str) -> bool {
            room == self.0.room
        }

        fn snapshot(&self, room: &str) -> Result<WorkspaceTaskSnapshot, String> {
            (room == self.0.room)
                .then(|| self.0.clone())
                .ok_or_else(|| "wrong test room".into())
        }

        fn host_certificate(&self, _room: &str) -> Result<tilo_user_identity::DeviceCert, String> {
            Err("read-only fixture has no host authority".into())
        }

        fn execute(
            &self,
            _request: tilo_peer_state::owner_commit::OwnerCommitRequest,
        ) -> tilo_peer_state::owner_commit::CommitFuture {
            Box::pin(async { Err("read-only fixture cannot mutate".into()) })
        }
    }

    fn task_provider_fixture() -> (
        Arc<FanoutState>,
        Arc<CapnpBrokerState>,
        String,
        tilo_actions::remote::Caller,
    ) {
        let user = [73; 32];
        let mut workspace = Workspace::new(WorkspaceId(73), "Task provider");
        workspace.owner = Some(hex::encode(user));
        let room = workspace.room.to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let service: Arc<dyn WorkspaceTasks> = Arc::new(ReadOnlyTasks(WorkspaceTaskSnapshot {
            room: room.clone(),
            generation: 7,
            tasks: Vec::new(),
            controls: std::collections::BTreeMap::new(),
        }));
        let fanout = Arc::new(FanoutState::new());
        let (state, _bridge) = CapnpBrokerState::new_configured(
            &fanout,
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.shared().authority()),
            None,
            None,
            HostServices {
                workspace_tasks: Some(service),
                ..Default::default()
            },
        );
        state.refresh_workspace_task_services().unwrap();
        let provider = tilo_actions::remote::workspace_provider(&room);
        let caller = tilo_actions::remote::Caller::authenticated(user, [74; 32])
            .in_room(room)
            .in_new_execution();
        (fanout, state, provider, caller)
    }

    fn task_request(provider: String) -> tilo_actions::remote::InvokeRequest {
        tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider,
                address: tilo_actions::ActionAddress::new("desktop:tasks", "read-current-tasks"),
            },
            argument: tilo_actions::remote::InvokeArgument::Json(serde_json::Map::from_iter([(
                "agent".into(),
                serde_json::json!("scribe"),
            )])),
            context: None,
            confirmation: None,
            revision: None,
            fresh_binding: None,
            on_behalf_of: None,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn workspace_provider_routes_the_sdk_task_target_to_the_exact_owner_service() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (_fanout, state, provider, caller) = task_provider_fixture();
                let row = state
                    .eager_catalogue(&caller, true)
                    .0
                    .into_iter()
                    .find(|row| {
                        row.provider == provider
                            && row.action.as_ref().is_some_and(|action| {
                                action.address.resource == "desktop:tasks"
                                    && action.address.action == "read-current-tasks"
                            })
                    })
                    .expect("the Task Center row uses the SDK's stable workspace provider");
                assert_eq!(row.app_id, "so.tilo.desktop");
                let request = task_request(provider);
                let mut unsupported_fresh = request.clone();
                unsupported_fresh.invocation = tilo_actions::InvocationId::new();
                unsupported_fresh.fresh_binding = Some(FreshActionBinding {
                    descriptor: row.action.clone().expect("task row has an action"),
                    operation_type: None,
                });
                assert!(matches!(
                    state
                        .action_invoke(&caller, None, None, &unsupported_fresh)
                        .poll_ready(),
                    tilo_actions::remote::PendingPoll::Ready(Err(
                        ActionHostError::InvalidRequest { .. }
                    ))
                ));
                assert!(
                    state
                        .remote_job(&caller, &unsupported_fresh.invocation)
                        .is_none(),
                    "a non-directory fresh binding must not reserve or dispatch an invocation"
                );
                let mut missing_required = request.clone();
                missing_required.invocation = tilo_actions::InvocationId::new();
                missing_required.argument = tilo_actions::remote::InvokeArgument::None;
                assert!(matches!(
                    state
                        .action_invoke(&caller, None, None, &missing_required)
                        .poll_ready(),
                    tilo_actions::remote::PendingPoll::Ready(Err(
                        ActionHostError::InvalidRequest { .. }
                    ))
                ));
                assert!(
                    state
                        .remote_job(&caller, &missing_required.invocation)
                        .is_none(),
                    "a missing required argument cannot reserve an invocation"
                );
                let mut pending = state.action_invoke(&caller, None, None, &request);
                let receipt = loop {
                    if let tilo_actions::remote::PendingPoll::Ready(result) = pending.poll_ready() {
                        break result.unwrap();
                    }
                    tokio::task::yield_now().await;
                };
                assert_eq!(receipt.invocation, request.invocation);
                let update = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                    loop {
                        if let Some(state) = state.action_observe(&caller, &request.invocation)
                            && state.update.is_terminal()
                        {
                            break state.update;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("the stable provider reaches the real owner service");
                let tilo_actions::InvocationUpdate::Succeeded { output, .. } = update else {
                    panic!("owner-backed read-current-tasks must succeed")
                };
                assert_eq!(output.value.unwrap()["generation"], 7);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn published_text_input_reaches_the_production_dispatch_path() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (_fanout, state, provider, caller) = task_provider_fixture();
                let peer = *state
                    .workspace_task_peers
                    .lock()
                    .unwrap()
                    .values()
                    .next()
                    .expect("fixture publishes one owner service");
                let mut surfaces = state.surfaces.lock().unwrap();
                let surface = surfaces.get_mut(&peer).expect("owner surface remains live");
                let mut text_action = surface
                    .actions
                    .iter()
                    .find(|action| {
                        action.resource == "desktop:tasks" && action.action == "read-current-tasks"
                    })
                    .expect("fixture publishes its task reader")
                    .clone();
                text_action.action = "text-probe".into();
                text_action.action_label = "Text probe".into();
                text_action.params.clear();
                text_action.input = super::super::PublishedInput::Text {
                    prompt: "Input".into(),
                    placeholder: String::new(),
                    wants_content: false,
                };
                surface.actions.push(text_action);
                drop(surfaces);

                let request = tilo_actions::remote::InvokeRequest {
                    invocation: tilo_actions::InvocationId::new(),
                    target: tilo_actions::remote::RemoteActionTarget {
                        provider,
                        address: tilo_actions::ActionAddress::new("desktop:tasks", "text-probe"),
                    },
                    argument: tilo_actions::remote::InvokeArgument::Text("hello".into()),
                    context: None,
                    confirmation: None,
                    revision: None,
                    fresh_binding: None,
                    on_behalf_of: None,
                };
                assert!(matches!(
                    state
                        .action_invoke(&caller, None, None, &request)
                        .poll_ready(),
                    tilo_actions::remote::PendingPoll::Ready(Ok(_))
                ));
                assert!(state.remote_job(&caller, &request.invocation).is_some());
            })
            .await;
    }
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
    use super::super::{BrokerChannels, CapnpBrokerState, FanoutState, PeerPublisher};
    use super::{CapDecision, ResourceRef, Verb};
    use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
    use std::sync::{Arc, Mutex};
    use tilo_actions::remote::Caller;

    /// Every room is governed by one device.
    struct GovernedBy([u8; 32]);

    impl crate::peer_actions::CertificateAuthority for GovernedBy {
        fn authorize(
            &self,
            _room: uuid::Uuid,
            _certificate: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            Ok(())
        }
        fn governs(&self, _room: uuid::Uuid, device: Option<&[u8; 32]>) -> bool {
            device == Some(&self.0)
        }
    }

    #[test]
    fn a_foreign_caller_is_capped_only_in_rooms_its_device_does_not_govern() {
        let user = [26; 32];
        let mut workspace = Workspace::new(WorkspaceId(6), "Callers");
        workspace.owner = Some(hex::encode(user));
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge_rx) = CapnpBrokerState::new(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.shared()),
            None,
        );
        let governor = [7; 32];
        let authority: Arc<dyn crate::peer_actions::CertificateAuthority> =
            Arc::new(GovernedBy(governor));
        assert!(state.certificate_authority.set(authority).is_ok());
        let may_administer = |caller: &Caller| {
            let (workspace, policy, now) = state.workspaces_for(caller).pop().unwrap();
            workspace.decide_at(&policy, &ResourceRef::Workspace, Verb::Admin, now)
                == CapDecision::Allow
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
    use super::super::{
        BrokerActor, BrokerChannels, CapnpBrokerState, FanoutState, PeerPublisher, PublishedSurface,
    };
    use crate::route::CrdtRoute;
    use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
    use std::sync::{Arc, Mutex};
    use tilo_actions::remote::Caller;

    #[derive(Default)]
    struct Launcher {
        closes: bool,
        closed: Mutex<Vec<(String, String)>>,
    }

    impl crate::services::LauncherServices for Launcher {
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            vec![crate::services::LaunchableApplication {
                package_id: "so.tilo.word-processor".into(),
                display_name: "Writer".into(),
                unconfined: false,
            }]
        }
        fn launch(&self, _: &str, _: &tilo_desktop_model::Workspace) -> Result<(), String> {
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
                .push((app.to_string(), workspace.room.to_string()));
            Ok(())
        }
    }

    fn broker(launcher: Arc<Launcher>, live: bool) -> (Arc<CapnpBrokerState>, Caller, String) {
        let user = [31; 32];
        let mut workspace = Workspace::new(WorkspaceId(8), "Close");
        workspace.owner = Some(hex::encode(user));
        let room = workspace.room.to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge_rx) = CapnpBrokerState::new_configured(
            &Arc::new(FanoutState::new()),
            Arc::new(Mutex::new(BrokerChannels::default())),
            PeerPublisher::disabled(),
            Some(tree.shared().authority()),
            None,
            None,
            crate::services::HostServices {
                launcher: Some(launcher),
                ..Default::default()
            },
        );
        if live {
            state.replace_published_surfaces_for_test(vec![PublishedSurface {
                peer_id: 4,
                app_id: "so.tilo.word-processor".into(),
                route: CrdtRoute::record("so.tilo.word-processor", &room, "doc"),
                actions: Vec::new(),
                live: true,
            }]);
        }
        let caller = Caller::authenticated(user, [32; 32]).in_room(room.clone());
        (state, caller, room)
    }

    fn app_actions(state: &CapnpBrokerState, caller: &Caller) -> Vec<String> {
        state
            .eager_catalogue(caller, true)
            .0
            .into_iter()
            .filter_map(|row| row.action)
            .filter(|action| action.address.resource == "app.so.tilo.word-processor")
            .map(|action| action.address.action)
            .collect()
    }

    #[test]
    fn a_running_app_offers_close_where_the_host_can_close_it() {
        let launcher = Arc::new(Launcher {
            closes: true,
            ..Default::default()
        });
        let (state, caller, _) = broker(Arc::clone(&launcher), false);
        assert_eq!(app_actions(&state, &caller), ["open"]);
        let (state, caller, room) = broker(Arc::clone(&launcher), true);
        assert_eq!(app_actions(&state, &caller), ["close"]);

        let (workspace, policy, now) = state.workspaces_for(&caller).pop().unwrap();
        let job = state
            .close_application(
                &workspace,
                &policy,
                now,
                &BrokerActor::person(hex::encode([31; 32])),
                "so.tilo.word-processor",
            )
            .unwrap();
        assert!(matches!(
            state.invocation_status(job),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
        assert_eq!(
            *launcher.closed.lock().unwrap(),
            [("so.tilo.word-processor".to_string(), room)]
        );

        let unsupported = Arc::new(Launcher::default());
        let (state, caller, _) = broker(unsupported, true);
        assert!(app_actions(&state, &caller).is_empty());
        let (workspace, policy, now) = state.workspaces_for(&caller).pop().unwrap();
        assert!(
            state
                .close_application(
                    &workspace,
                    &policy,
                    now,
                    &BrokerActor::person("someone"),
                    "so.tilo.word-processor",
                )
                .is_err()
        );
    }

    #[test]
    fn a_caller_who_may_not_change_the_app_cannot_close_it() {
        let launcher = Arc::new(Launcher {
            closes: true,
            ..Default::default()
        });
        let (state, _, room) = broker(Arc::clone(&launcher), true);
        let stranger = Caller::authenticated([99; 32], [98; 32]).in_room(room);
        assert!(app_actions(&state, &stranger).is_empty());
        assert!(launcher.closed.lock().unwrap().is_empty());
    }
}

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
