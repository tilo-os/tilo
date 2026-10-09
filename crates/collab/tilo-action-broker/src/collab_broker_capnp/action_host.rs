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
    APPLICATION_CLOSE_ACTION, APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX,
    ActionHostError, Caller, CallerPrincipal, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, Completer, Confirmation, InvocationReceipt, InvocationState,
    InvokeRequest, OWN_PROVIDER, Pending, ReceiptStatus, Reservation, application_provider,
    pending, workspace_provider, workspace_provider_room,
};
use tilo_actions::{ActionAddress, ActionDescriptor, Collects, ExecutionLocus, InputSpec};

use super::launchable::{is_launchable_provider, launchable_provider};
use super::{
    ActionDirectoryBridgeError, ActionDirectoryBridgeReply, ActionDirectoryBridgeResponse,
    ActionDirectoryRoot, BrokerActor, CapnpBrokerState, DIRECTORY_MAX_PARAMS_BYTES,
    InvocationInputs, InvocationOwner, LaunchScope, PublishedAction, PublishedInput,
    PublishedSurface, app_node, confirmation_satisfied, published_directory_action,
    validate_directory_target_input,
};
use crate::automation;
use crate::collab_broker::CrdtRoute;
use crate::services::LaunchPlace;
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
            CallerPrincipal::Agent(id) => {
                let agent = Self::agent(format!("agent:{id}"));
                match caller.user_public_key() {
                    Some(root) => agent.for_person(super::principal_id_for_root(*root)),
                    None => agent,
                }
            }
            CallerPrincipal::App(id) => Self::agent(format!("app:{id}")),
        }
    }
}

/// The broker as the [`ActionHost`](tilo_actions::remote::ActionHost) an
/// admitted application or agent connection reaches over its channel's
/// `remoteAction` door. [`OWN_PROVIDER`] names that connection itself.
#[derive(Clone)]
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

/// Which rows a [`CapnpBrokerState::eager_catalogue`] pass builds. Every
/// scope lists the same providers and puts each row it builds through the
/// same checks; a narrower one only skips the rows of providers nobody asked
/// about.
#[derive(Clone, Copy, Debug)]
pub(super) enum RowScope<'a> {
    /// Who publishes, and no rows.
    None,
    /// Every row the caller is offered.
    All,
    /// The rows of this one provider.
    Provider(&'a str),
}

impl RowScope<'_> {
    /// Whether rows of `provider` are built.
    fn wants(self, provider: &str) -> bool {
        match self {
            Self::None => false,
            Self::All => true,
            Self::Provider(wanted) => wanted == provider,
        }
    }

    /// Whether rows of the live instance `peer_id` are built.
    fn wants_peer(self, peer_id: u64) -> bool {
        match self {
            Self::None => false,
            Self::All => true,
            Self::Provider(wanted) => wanted == peer_id.to_string(),
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
fn refusal(reason: String) -> ActionHostError {
    if reason.starts_with("action_confirmation_required")
        || reason.contains("confirmation is required")
    {
        ActionHostError::ConfirmationRequired
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
            RemoteActionMessage::Observe { invocation, .. }
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

    /// Whether `caller` may act in `workspace`. A caller confined to a room
    /// was admitted to it by its connection; any other caller must be a
    /// member.
    fn may_act_in(caller: &Caller, workspace: &Workspace, now: u64) -> bool {
        match caller.room() {
            Some(confined) => confined == workspace.room.to_string(),
            None => match caller.principal() {
                // An application is always admitted into one room;
                // unconfined, it takes part in none.
                CallerPrincipal::App(_) => false,
                // Anyone else takes part in a room they may read; a foreign
                // device reads wherever its person does.
                CallerPrincipal::Person(_) | CallerPrincipal::Agent(_) => {
                    BrokerActor::for_caller(caller).reads_in(workspace, now)
                }
            },
        }
    }

    /// One policy sample of `room` for `caller`: `decide` runs once on the
    /// borrowed committed workspace with the caller's policy actor there, or
    /// not at all (`None`) when `caller` may not act in `room`. Nothing is
    /// copied; `decide` must not block or call host services.
    fn decide_for<R>(
        &self,
        caller: &Caller,
        room: &str,
        decide: impl FnOnce(&Workspace, &BrokerActor, u64) -> R,
    ) -> Option<R> {
        let membership = self.membership.as_ref()?;
        // An unavailable clock offers nothing rather than deciding at an
        // unknown time.
        let now = self.policy_now_ns().ok()? / 1_000_000_000;
        // The actor consults the certificate authority: asked before the
        // workspace is borrowed, never under it.
        let actor = self.caller_actor(caller, room);
        membership
            .decide_in_room(room, |workspace| {
                let workspace =
                    workspace.filter(|workspace| Self::may_act_in(caller, workspace, now))?;
                Some(decide(workspace, &actor, now))
            })
            .ok()
            .flatten()
    }

    /// The rooms `caller` may act in, decided on one borrowed sample.
    fn rooms_for(&self, caller: &Caller) -> Vec<String> {
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
                    .map(|workspace| workspace.room.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    /// Owned copies of the workspaces `caller` may act in, with the actor
    /// it is in each, all from one sample. Only for a pass that hands each
    /// workspace to host services (which read the host's authority
    /// themselves) and so cannot run under the borrow; only the caller's
    /// own workspaces are copied.
    fn workspaces_for(&self, caller: &Caller) -> Vec<(Workspace, BrokerActor, u64)> {
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
                let actor = self.caller_actor(caller, &workspace.room.to_string());
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
        let node = Self::target_node(workspace, app, target, contract);
        actor.answer_in(workspace, &node, contract.kind, now) != tilo_action_policy::Answer::Deny
    }

    /// Whether `actor` may take the action `target` now. See
    /// [`crate::published_authorization::permits`].
    pub(super) fn allowed(
        workspace: &Workspace,
        actor: &BrokerActor,
        now: u64,
        app: &str,
        target: (&str, &str, &str),
        contract: &tilo_actions::ActionContract,
    ) -> bool {
        let node = Self::target_node(workspace, app, target, contract);
        actor.may_in(workspace, &node, contract.kind, now)
    }

    fn target_node(
        workspace: &Workspace,
        app: &str,
        (resource_kind, resource, action): (&str, &str, &str),
        contract: &tilo_actions::ActionContract,
    ) -> tilo_action_policy::TreePath {
        tilo_actions::action_node(
            &workspace.node(),
            app,
            resource_kind,
            resource,
            contract.field.as_deref(),
            action,
        )
    }

    /// Every live published surface, carrying its actions only where
    /// `scope` builds that surface's rows: who publishes is copied, not every
    /// app's whole surface, when that is all a caller asked.
    fn live_surfaces(&self, scope: RowScope<'_>) -> Vec<PublishedSurface> {
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
                        actions: if scope.wants_peer(surface.peer_id) {
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
    /// A narrower scope only leaves other providers' rows out: each row it
    /// builds passes the same checks, and comes out the same, as in
    /// [`RowScope::All`]. The installed applications and the keyboard
    /// shortcut settings are read at most once, and only when a workspace
    /// provider's rows are built.
    pub(super) fn eager_catalogue(
        &self,
        caller: &Caller,
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
        for (workspace, policy, now) in self.workspaces_for(caller) {
            let room = workspace.room.to_string();
            let mut live_apps = std::collections::HashSet::new();
            for surface in surfaces
                .iter()
                .filter(|surface| surface.live && surface.route.cluster_id == room)
            {
                live_apps.insert(surface.app_id.clone());
                providers.push(CatalogueProvider {
                    provider: surface.peer_id.to_string(),
                    app_id: surface.app_id.clone(),
                    room: room.clone(),
                    record_id: surface.route.record_id.clone(),
                    browsable: roots.iter().any(|root| root.peer_id == surface.peer_id),
                });
                if !scope.wants_peer(surface.peer_id) {
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
                            action_row(
                                surface.peer_id.to_string(),
                                &surface.app_id,
                                descriptor(action),
                            )
                        }),
                );
            }
            if caller.room().is_some() {
                self.launchable_catalogue(
                    &workspace,
                    &policy,
                    now,
                    &live_apps,
                    (scope, &mut rows),
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
            let provider = workspace_provider(&room);
            if !scope.wants(&provider) {
                continue;
            }
            if let Some(governance) = self.services.workspace_governance.as_ref() {
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
                            action_row(provider.clone(), governance.app_id(), descriptor)
                        }),
                );
            }
            if self.services.shortcut_library.is_some() {
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
                            action_row(provider.clone(), SHORTCUTS_APP_ID, descriptor)
                        }),
                );
            }
            if let Some(service) = keyboard {
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
            if caller.room().is_some()
                && let Some(launcher) = self.services.launcher.as_ref()
            {
                let applications = applications.get_or_insert_with(|| {
                    (launcher.applications(), launcher.closes_applications())
                });
                let (applications, closes_applications) = (&applications.0, applications.1);
                rows.extend(
                    applications
                        .iter()
                        .filter(|app| !live_apps.contains(&app.package_id))
                        .filter(|app| {
                            policy.may_in(
                                &workspace,
                                &app_node(&workspace, &app.package_id),
                                tilo_action_policy::ActionKind::Undoable,
                                now,
                            )
                        })
                        .map(|app| {
                            action_row(provider.clone(), &app.package_id, app_open_descriptor(app))
                        }),
                );
                rows.extend(
                    applications
                        .iter()
                        .filter(|_| closes_applications)
                        .filter(|app| live_apps.contains(&app.package_id))
                        .filter(|app| {
                            policy.may_in(
                                &workspace,
                                &app_node(&workspace, &app.package_id),
                                tilo_action_policy::ActionKind::Undoable,
                                now,
                            )
                        })
                        .map(|app| {
                            action_row(
                                provider.clone(),
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
        policy: &BrokerActor,
        now: u64,
        live_apps: &std::collections::HashSet<String>,
        (scope, rows): (RowScope<'_>, &mut Vec<CatalogueRow>),
        providers: &mut Vec<CatalogueProvider>,
    ) {
        let room = workspace.room.to_string();
        for surface in self.launchable.surfaces().into_iter().filter(|surface| {
            surface.route.cluster_id == room
                && !live_apps.contains(&surface.app_id)
                && policy.may_in(
                    workspace,
                    &app_node(workspace, &surface.app_id),
                    tilo_action_policy::ActionKind::Undoable,
                    now,
                )
        }) {
            let provider = launchable_provider(&surface);
            if scope.wants(&provider) {
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
        let rooms = self.rooms_for(caller);
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
        // `max_results: 0` asks only who publishes, and a named provider
        // only its own rows: build no row nobody asked for.
        let scope = match query.provider.as_deref() {
            Some(provider) => RowScope::Provider(provider),
            None if query.max_results == 0 => RowScope::None,
            None => RowScope::All,
        };
        let (rows, providers) = self.eager_catalogue(caller, scope);
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
        if self.decide_for(caller, &route.room, |_, _, _| ()).is_none() {
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
        let reply = if let Some(target) = &query.target {
            let Some(revision) = query
                .revision
                .clone()
                .filter(|revision| !revision.is_empty())
            else {
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
                directory_page(&root, reply)
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
        // The launcher keeps the workspace past the read: only this room's
        // workspace is copied, once every check on the same sample passed.
        let workspace = self
            .decide_for(caller, &room, |workspace, policy, now| {
                if !policy.may_in(
                    workspace,
                    &app_node(workspace, &app_id),
                    tilo_action_policy::ActionKind::Undoable,
                    now,
                ) {
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
                    if !confirmation_satisfied(action, request.confirmation.as_ref()) {
                        return Err(ActionHostError::ConfirmationRequired);
                    }
                }
                Ok(workspace.clone())
            })
            .ok_or(ActionHostError::UnknownAction)??;
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
                let place = self.launch_place(caller, &room, None);
                // The package's room record is what a plain launch opens.
                let record = destination
                    .record
                    .clone()
                    .filter(|record| !record.is_empty());
                tokio::task::spawn_blocking(move || {
                    state.launch_retained(&app, &current.workspace, record.as_deref(), &place)
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

    /// Start `app_id` in `workspace` on `record` at `place` through the
    /// host's launcher, or the one a test installed in its place.
    fn launch_retained(
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

    /// Where a view `caller`'s action needs opens in `room`: beside the
    /// asking app's own view; where a person is looking, for the person's
    /// own invocation; else beside the view the actor last worked in, so an
    /// agent or a Shortcut keeps working next to where it already is.
    fn launch_place(
        &self,
        caller: &Caller,
        room: &str,
        launch: Option<&LaunchScope>,
    ) -> LaunchPlace {
        if let Some(scope) = launch {
            return LaunchPlace::Beside {
                app_id: scope.app_id.clone(),
            };
        }
        match caller.principal() {
            CallerPrincipal::Person(_) => LaunchPlace::Focus,
            CallerPrincipal::App(app_id) => LaunchPlace::Beside {
                app_id: app_id.clone(),
            },
            CallerPrincipal::Agent(_) => self
                .launchable
                .view_of(room, &BrokerActor::for_caller(caller).id)
                .map_or(LaunchPlace::Focus, |app_id| LaunchPlace::Beside { app_id }),
        }
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
        let unavailable = || ActionHostError::Unavailable {
            reason: "the invocation ledger is unavailable".into(),
        };
        // A request naming no revision acts on the directory as it is now:
        // its owner resolves it so, in the same turn this broker admits it,
        // instead of the caller first reading the revision across the same
        // two hops.
        let revision = match request.revision.as_deref() {
            None => String::new(),
            Some("") => {
                return Pending::ready(Err(ActionHostError::InvalidRequest {
                    reason:
                        "an empty revision names none; omit it to act on the directory as it is now"
                            .into(),
                }));
            }
            Some(revision) => revision.to_owned(),
        };
        let argument = request.argument.to_slot();
        let address = &request.target.address;
        if let Some(reason) = argument.as_deref().and_then(oversized_argument) {
            return Pending::ready(Err(ActionHostError::InvalidRequest { reason }));
        }
        if validate_directory_target_input(
            &root.cluster_id,
            &revision,
            &address.resource,
            &address.action,
            argument.as_deref(),
            None,
        )
        .is_err()
        {
            return Pending::ready(Err(ActionHostError::InvalidRequest {
                reason: "the directory target or argument is malformed".into(),
            }));
        }
        let reservation = match self.remote_invocations.lock() {
            Ok(mut ledger) => ledger.reserve(caller, request),
            Err(_) => Err(unavailable()),
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
        let resolved = self.action_directory_request_queue().try_resolve_pending(
            self.caller_actor(caller, &root.cluster_id),
            &root.cluster_id,
            root.peer_id,
            revision,
            &address.resource,
            &address.action,
        );
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
        let row = match reply {
            Some(Ok(ActionDirectoryBridgeResponse::Resolution(
                runtime_collab::ActionDirectoryResolution::Found(row),
            ))) => row,
            Some(Ok(ActionDirectoryBridgeResponse::Resolution(
                runtime_collab::ActionDirectoryResolution::StaleRevision,
            ))) => {
                return Err(ActionHostError::InvalidRequest {
                    reason: "the action changed since that revision; list it again".into(),
                });
            }
            Some(Ok(_)) => return Err(ActionHostError::UnknownAction),
            Some(Err(error)) => return Err(bridge_error(error)),
            None => {
                return Err(ActionHostError::Unavailable {
                    reason: "the action directory went away".into(),
                });
            }
        };
        let offered =
            published_directory_action(&row).map_err(|_| ActionHostError::UnknownAction)?;
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
                argument,
                actor: &actor,
                confirmation_token: token.as_deref(),
                context: request.context.clone(),
            },
            super::AdmissionProvenance {
                durable: None,
                revision: request.revision.as_deref(),
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
        if let Some(room) = workspace_provider_room(provider) {
            return self.start_workspace_action(
                caller,
                &self.caller_actor(caller, room),
                room,
                launch,
                request,
                argument.as_deref(),
            );
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
            .decide_for(caller, &room, |workspace, policy, now| {
                Self::allowed(
                    workspace,
                    policy,
                    now,
                    &app_id,
                    (&action.resource_kind, &action.resource, &action.action),
                    &action.contract,
                )
            })
            .and_then(|allowed| allowed.then_some((action, room)))
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
                argument: argument.as_deref(),
                actor: &actor,
                confirmation_token: token.as_deref(),
                context: request.context.clone(),
            },
            origin.cloned(),
        )
        .map(|job| {
            self.launchable.note_view(&room, &actor.id, &app_id);
            job.id
        })
        .map_err(refusal)
    }

    fn start_workspace_action(
        &self,
        caller: &Caller,
        actor: &BrokerActor,
        room: &str,
        launch: Option<&LaunchScope>,
        request: &InvokeRequest,
        argument: Option<&str>,
    ) -> Result<u64, ActionHostError> {
        let address = &request.target.address;
        // Workspace actions hand the workspace to host services (launcher,
        // shortcut library), which cannot run under the borrow: copy only
        // this one room's workspace.
        let Some((workspace, policy, now)) =
            self.decide_for(caller, room, |workspace, policy, now| {
                (workspace.clone(), policy.clone(), now)
            })
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
                    if policy.may_in(
                        &workspace,
                        &workspace.node(),
                        tilo_action_policy::ActionKind::Undoable,
                        now,
                    ) =>
                {
                    Some(person.as_str())
                }
                _ => None,
            };
            return self
                .launch_application(
                    (&workspace, &policy, now),
                    actor,
                    app_id,
                    crate::services::LaunchContent {
                        hash: launch_content_hash(argument),
                        caller: launch,
                        author,
                    },
                    &self.launch_place(caller, room, launch),
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
            return self
                .invoke_workspace_governance_action(
                    room,
                    actor,
                    &address.resource,
                    &address.action,
                    argument,
                    &confirmation_evidence(request.confirmation.as_ref()),
                )
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
        (workspace, policy, now): (&Workspace, &BrokerActor, u64),
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
            (
                &descriptor.resource_kind,
                &descriptor.address.resource,
                action,
            ),
            &descriptor.contract,
        ) {
            return Err(ActionHostError::NotPermitted);
        }
        let operation = automation::decode_library(action, argument)
            .map_err(|reason| ActionHostError::InvalidRequest { reason })?;
        let catalog =
            tilo_automation::ActionCatalog::new(self.eager_catalogue(caller, RowScope::All).0);
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
        (workspace, policy, now): (&Workspace, &BrokerActor, u64),
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
            (
                &descriptor.resource_kind,
                &address.resource,
                &address.action,
            ),
            &descriptor.contract,
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
        let catalog =
            tilo_automation::ActionCatalog::new(self.eager_catalogue(caller, RowScope::All).0);
        let others = self
            .decide_for(caller, room, |workspace, _, _| workspace.shortcuts.clone())
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
        self.decide_for(caller, room, |workspace, _, _| {
            workspace
                .shortcuts
                .iter()
                .find(|shortcut| shortcut.id == shortcut_id)
                .cloned()
        })
        .flatten()
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
        let state = Arc::clone(self);
        let caller = caller.clone().running_shortcut(shortcut.id.clone());
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
        resource: &str,
        action: &str,
        argument: Option<&str>,
        confirmation: &serde_json::Value,
    ) -> Result<u64, String> {
        let governance = self
            .services
            .workspace_governance
            .as_ref()
            .ok_or_else(|| "workspace governance is unavailable on this host".to_string())?;
        let id = self
            .invocation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        governance.invoke(&crate::services::WorkspaceGovernanceRequest {
            room,
            actor,
            resource,
            action,
            argument,
            confirmation,
            invocation,
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

    /// Launch `app_id` into `workspace`, optionally opening content-addressed
    /// bytes read-only, and record the terminal outcome as an invocation.
    ///
    /// # Errors
    ///
    /// Returns a message when the caller may not launch the application or no launcher is installed.
    pub fn launch_application(
        &self,
        (workspace, policy, now): (&Workspace, &BrokerActor, u64),
        actor: &BrokerActor,
        app_id: &str,
        content: crate::services::LaunchContent<'_>,
        place: &LaunchPlace,
    ) -> Result<u64, String> {
        if !policy.may_in(
            workspace,
            &app_node(workspace, app_id),
            tilo_action_policy::ActionKind::Undoable,
            now,
        ) {
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
        let update = match launcher.launch_content(app_id, workspace, content, place) {
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
        policy: &BrokerActor,
        now: u64,
        actor: &BrokerActor,
        app_id: &str,
    ) -> Result<u64, String> {
        if !policy.may_in(
            workspace,
            &app_node(workspace, app_id),
            tilo_action_policy::ActionKind::Undoable,
            now,
        ) {
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
        self.request_cancellation(job, &BrokerActor::for_caller(caller).id)
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
        has_children: false,
        visibility: None,
    }
}

/// A directory row, already validated and permission-checked on the local
/// set, as a catalogue row.
fn directory_row(
    root: &ActionDirectoryRoot,
    row: &runtime_collab::ActionDirectoryRow,
) -> Option<CatalogueRow> {
    let action = if row.action.is_some() {
        Some(descriptor(&published_directory_action(row).ok()?))
    } else {
        None
    };
    Some(CatalogueRow {
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
        has_children: row.has_children,
        visibility: row.visibility.clone(),
    })
}

fn directory_page(
    root: &ActionDirectoryRoot,
    reply: ActionDirectoryBridgeReply,
) -> Result<CataloguePage, ActionHostError> {
    match reply.map_err(bridge_error)? {
        ActionDirectoryBridgeResponse::Page(page) => Ok(CataloguePage {
            locator: None,
            rows: page
                .rows
                .iter()
                .filter_map(|row| directory_row(root, row))
                .collect(),
            revision: page.revision,
            providers: Vec::new(),
            next_cursor: page.next_cursor,
            exhausted: page.exhausted,
            stale_cursor: page.stale_cursor,
            budget_exhausted: page.budget_exhausted,
        }),
        ActionDirectoryBridgeResponse::Resolution(resolution) => {
            let mut page = CataloguePage {
                exhausted: true,
                ..CataloguePage::default()
            };
            match resolution {
                runtime_collab::ActionDirectoryResolution::Found(row) => {
                    page.rows.extend(directory_row(root, &row));
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
    use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
    use std::sync::{Arc, Mutex};
    use tilo_action_policy::roles;
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
        workspace.set_owner(hex::encode(user));
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
            policy.may_in(
                &workspace,
                &roles::governance_action(&workspace.policy.room, roles::governance::SETTINGS),
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
    use super::super::{
        BrokerActor, BrokerChannels, CapnpBrokerState, FanoutState, PeerPublisher, PublishedSurface,
    };
    use super::RowScope;
    use crate::route::CrdtRoute;
    use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
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
                package_id: "so.tilo.word-processor".into(),
                display_name: "Writer".into(),
                unconfined: false,
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
                .push((app.to_string(), workspace.room.to_string()));
            Ok(())
        }
    }

    fn broker(launcher: Arc<Launcher>, live: bool) -> (Arc<CapnpBrokerState>, Caller, String) {
        let user = [31; 32];
        let mut workspace = Workspace::new(WorkspaceId(8), "Close");
        workspace.set_owner(hex::encode(user));
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
            .eager_catalogue(caller, RowScope::All)
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

    /// The page `query` answers `caller`, and how often it read the
    /// installed-application list.
    fn answered(
        state: &Arc<CapnpBrokerState>,
        launcher: &Launcher,
        caller: &Caller,
        query: &tilo_actions::remote::CatalogueQuery,
    ) -> (tilo_actions::remote::CataloguePage, usize) {
        let before = launcher.listed.load(std::sync::atomic::Ordering::SeqCst);
        let page = state
            .action_query(caller, query)
            .wait_timeout(std::time::Duration::from_secs(5))
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
        let (state, caller, room) = broker(Arc::clone(&launcher), true);
        let workspace = tilo_actions::remote::workspace_provider(&room);
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
        // The workspace's own rows read it once, for open and close alike.
        let (page, listed) = answered(&state, &launcher, &caller, &query(Some(&workspace), 64));
        assert_eq!(listed, 1);
        assert!(page.rows.iter().all(|row| row.provider == workspace));
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
        for provider in ["4", workspace.as_str()] {
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
        let stranger = Caller::authenticated([99; 32], [98; 32]).in_room(room);
        let (rows, _) = state.eager_catalogue(&stranger, RowScope::Provider(&workspace));
        assert!(rows.is_empty());
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
