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
    CatalogueResource, CatalogueRow, Completer, Confirmation, InvocationReceipt, InvocationState,
    InvokeRequest, OWN_PROVIDER, Pending, ReceiptStatus, Reservation, RowVisibility, pending,
    workspace_provider, workspace_provider_room,
};
use tilo_actions::{ActionAddress, ActionDescriptor, Collects, ExecutionLocus, InputSpec};

use super::launchable::{is_launchable_provider, launchable_provider};
use super::{
    ActionDirectoryBridgeError, ActionDirectoryBridgeReply, ActionDirectoryBridgeResponse,
    ActionDirectoryRoot, BrokerActor, CapDecision, CapnpBrokerState, InvocationInputs,
    InvocationOwner, LaunchScope, PublishedAction, PublishedInput, PublishedSurface, ResourceRef,
    Verb, confirmation_satisfied, permission_verb, published_directory_action,
    validate_directory_target_input,
};
use crate::automation;
use crate::collab_broker::CrdtRoute;
use tilo_automation::SHORTCUTS_APP_ID;

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
struct BrokerDoor(Arc<CapnpBrokerState>);

impl tilo_actions::remote::ActionHost for BrokerDoor {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.0.action_query(caller, &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.0.action_invoke(caller, None, None, &request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.0.action_observe(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Result<(), ActionHostError> {
        self.0.action_cancel(caller, id)
    }
}

fn descriptor(action: &PublishedAction) -> ActionDescriptor {
    ActionDescriptor {
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
                        CallerPrincipal::Agent(id) => workspace.role_of_agent_at(id, now).is_some(),
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
                    action: action.to_string(),
                },
                verb,
                now,
            ) == CapDecision::Allow
        })
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
        let surfaces = self.published_surfaces();
        let roots = self.action_directory_roots();
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
                live_apps.insert(surface.app_id.clone());
                providers.push(CatalogueProvider {
                    provider: surface.peer_id.to_string(),
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
                                &action.action,
                                action.contract.permission,
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
                                    &descriptor.address.action,
                                    descriptor.contract.permission,
                                )
                            })
                        }),
                );
            }
            if caller.room().is_some() {
                rows.extend(
                    self.services
                        .launcher
                        .as_ref()
                        .map(|launcher| launcher.applications())
                        .unwrap_or_default()
                        .into_iter()
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
                                app_open_descriptor(&app),
                            )
                        }),
                );
                rows.extend(
                    self.services
                        .launcher
                        .as_ref()
                        .filter(|launcher| launcher.closes_applications())
                        .map(|launcher| launcher.applications())
                        .unwrap_or_default()
                        .into_iter()
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
        match reply {
            Ok(reply) => reply.map(move |reply| directory_page(&root, reply)),
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
        if is_launchable_provider(&request.target.provider) {
            return self.invoke_launchable(caller, request);
        }
        if let Some(root) = self.directory_target(caller, request) {
            return self.invoke_directory(caller, root, request);
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
        let (workspace, app_id) = match self.admit_launchable(caller, request) {
            Ok(admitted) => admitted,
            Err(error) => {
                if let Ok(mut ledger) = self.remote_invocations.lock() {
                    ledger.abandon(&request.invocation);
                }
                return Pending::ready(Err(error));
            }
        };
        let (completer, answer) = pending();
        let state = Arc::clone(self);
        let caller = caller.clone();
        let request = request.clone();
        let invocation = request.invocation;
        // Launching and waiting for the app block; they run beside the
        // broker, never on its local set.
        let spawned = std::thread::Builder::new()
            .name("tilo-launchable-invoke".into())
            .spawn(move || {
                let started =
                    state.relaunch_and_deliver(&completer, &caller, &workspace, &app_id, &request);
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
        if let Err(error) = spawned {
            if let Ok(mut ledger) = self.remote_invocations.lock() {
                ledger.abandon(&invocation);
            }
            return Pending::ready(Err(ActionHostError::Unavailable {
                reason: format!("the launch could not start: {error}"),
            }));
        }
        answer
    }

    /// Check a launchable invocation before anything starts: the caller may
    /// launch the app and use the retained action, and carries the
    /// confirmation its contract asks for, so a refusal never costs a launch.
    /// Returns the workspace to launch into and the application.
    fn admit_launchable(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(Workspace, String), ActionHostError> {
        let retained = self
            .launchable
            .surface_for(&request.target.provider)
            .ok_or(ActionHostError::UnknownAction)?;
        // Only a caller on this machine launches apps on this desktop.
        if caller.room().is_none() {
            return Err(ActionHostError::UnknownAction);
        }
        let (workspace, policy, now) = self
            .workspaces_for(caller)
            .into_iter()
            .find(|(workspace, _, _)| workspace.room.to_string() == retained.route.cluster_id)
            .ok_or(ActionHostError::UnknownAction)?;
        if workspace.decide_at(
            &policy,
            &ResourceRef::App(retained.app_id.clone()),
            Verb::Write,
            now,
        ) != CapDecision::Allow
        {
            return Err(refusal(
                "the caller is not allowed to launch that application".into(),
            ));
        }
        let address = &request.target.address;
        let action = retained
            .actions
            .iter()
            .find(|action| action.resource == address.resource && action.action == address.action)
            .filter(|action| {
                Self::allowed(
                    &workspace,
                    &policy,
                    now,
                    &retained.app_id,
                    &action.action,
                    action.contract.permission,
                )
            })
            .ok_or(ActionHostError::UnknownAction)?;
        if !confirmation_satisfied(action, request.confirmation.as_ref()) {
            return Err(ActionHostError::ConfirmationRequired);
        }
        Ok((workspace, retained.app_id))
    }

    /// Launch `app_id` unless it already publishes the action, then deliver
    /// the invocation to the connection that does, through the same
    /// admission a live action takes.
    fn relaunch_and_deliver(
        &self,
        completer: &Completer<Result<InvocationReceipt, ActionHostError>>,
        caller: &Caller,
        workspace: &Workspace,
        app_id: &str,
        request: &InvokeRequest,
    ) -> Result<u64, ActionHostError> {
        const POLL: std::time::Duration = std::time::Duration::from_millis(50);
        let room = workspace.room.to_string();
        let address = &request.target.address;
        let timeout = self.launchable.delivery_timeout();
        let deadline = std::time::Instant::now().checked_add(timeout);
        let mut launched = false;
        loop {
            if completer.is_abandoned() {
                return Err(ActionHostError::Unavailable {
                    reason: "the caller stopped waiting for admission".into(),
                });
            }
            if let Some(peer_id) = self.ready_publisher(app_id, &room, address) {
                let mut delivered = request.clone();
                delivered.target.provider = peer_id.to_string();
                return self.start_for(caller, None, None, None, &delivered);
            }
            if !launched {
                self.launch_retained(app_id, workspace).map_err(|reason| {
                    ActionHostError::Refused {
                        reason: format!("{app_id} could not be launched: {reason}"),
                    }
                })?;
                launched = true;
                continue;
            }
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                return Err(ActionHostError::Unavailable {
                    reason: format!(
                        "{app_id} was launched but did not offer \"{}\" within {:.1} s",
                        address.action,
                        timeout.as_secs_f32()
                    ),
                });
            }
            std::thread::sleep(POLL);
        }
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
        let eager = self.published_surfaces().into_iter().any(|surface| {
            surface.peer_id == peer_id
                && surface.live
                && surface.actions.iter().any(|action| {
                    action.resource == address.resource && action.action == address.action
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
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let unavailable = || ActionHostError::Unavailable {
            reason: "the invocation ledger is unavailable".into(),
        };
        let Some(revision) = request
            .revision
            .clone()
            .filter(|revision| !revision.is_empty())
        else {
            return Pending::ready(Err(ActionHostError::InvalidRequest {
                reason: "a directory action needs the revision it was chosen at".into(),
            }));
        };
        let argument = request.argument.to_slot();
        let address = &request.target.address;
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
                state.start_resolved(&caller, &root, &request, argument.as_deref(), reply)
            };
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
        admitted
    }

    fn start_resolved(
        &self,
        caller: &Caller,
        root: &ActionDirectoryRoot,
        request: &InvokeRequest,
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
        self.start_invocation_from_offered(
            root.peer_id,
            (surface, offered),
            InvocationInputs {
                argument,
                actor: &actor,
                confirmation_token: token.as_deref(),
                context: request.context.clone(),
            },
            request.revision.as_deref(),
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
        let (offered, room) = self
            .published_surfaces()
            .into_iter()
            .find(|surface| surface.peer_id == peer_id && surface.live)
            .and_then(|surface| {
                let (workspace, policy, now) =
                    self.workspaces_for(caller)
                        .into_iter()
                        .find(|(workspace, _, _)| {
                            workspace.room.to_string() == surface.route.cluster_id
                        })?;
                let action = surface
                    .actions
                    .iter()
                    .find(|action| {
                        action.resource == address.resource && action.action == address.action
                    })?
                    .clone();
                Self::allowed(
                    &workspace,
                    &policy,
                    now,
                    &surface.app_id,
                    &action.action,
                    action.contract.permission,
                )
                .then_some((action, surface.route.cluster_id))
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
                automation::DoorHooks::new(request.confirmation.clone()),
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
        let hooks = automation::SheetHooks::new(self.step_prompts.clone(), &shortcut.name);
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
            let hooks = automation::SheetHooks::new(self.step_prompts.clone(), &shortcut.name);
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
            automation::DoorHooks::new(None),
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
        hooks: K,
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
        let host = BrokerDoor(Arc::clone(self));
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
            presentation: serde_json::from_str::<tilo_actions::ResourcePresentation>(
                &row.resource_presentation_json,
            )
            .ok()
            .filter(tilo_actions::ResourcePresentation::is_valid),
        },
        action,
        has_children: row.has_children,
        visibility: row.visibility_action.clone().and_then(|action| {
            Some(RowVisibility {
                action,
                contract: serde_json::from_str(&row.visibility_contract_json).ok()?,
            })
        }),
    })
}

fn directory_page(
    root: &ActionDirectoryRoot,
    reply: ActionDirectoryBridgeReply,
) -> Result<CataloguePage, ActionHostError> {
    match reply.map_err(bridge_error)? {
        ActionDirectoryBridgeResponse::Page(page) => Ok(CataloguePage {
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
