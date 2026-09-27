//! The broker's answers to the remote-action vocabulary.
//!
//! Every caller that is not the application itself — a peer on the shell
//! network, an app or agent on this machine, the MCP bridge, the palette —
//! reaches published actions through these methods, wrapped as an
//! [`ActionHost`](tilo_actions::remote::ActionHost) by
//! [`BrokerActionHost`](crate::shell::remote_actions::BrokerActionHost).
//! Authority is decided from the transport-supplied [`Caller`] alone, with
//! the same capability checks the admission path has always applied.

use crate::shell::state::WorkspaceAuthority as _;
use std::sync::Arc;

use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionHostError, Caller, CallerPrincipal,
    CataloguePage, CatalogueProvider, CatalogueQuery, CatalogueResource, CatalogueRow, Completer,
    Confirmation, InvocationReceipt, InvocationState, InvokeRequest, OWN_PROVIDER, Pending,
    ReceiptStatus, Reservation, RowVisibility, pending, workspace_provider,
    workspace_provider_room,
};
use tilo_actions::{ActionAddress, ActionDescriptor, Collects, ExecutionLocus, InputSpec};

use super::launchable::{is_launchable_provider, launchable_provider};
use super::{
    ActionDirectoryBridgeError, ActionDirectoryBridgeReply, ActionDirectoryBridgeResponse,
    ActionDirectoryRoot, BrokerActor, CapDecision, CapnpBrokerState, LaunchScope, PublishedAction,
    PublishedInput, PublishedSurface, ResourceRef, SessionContentStore, Verb,
    confirmation_satisfied, permission_verb, published_directory_action,
    validate_directory_target_input,
};
use crate::shell::automation_host;
use crate::shell::collab_broker::CrdtRoute;
use crate::shell::keyboard_shortcuts;
use tilo_automation::SHORTCUTS_APP_ID;

impl BrokerActor {
    /// The broker actor a caller is admitted as.
    pub(crate) fn for_caller(caller: &Caller) -> Self {
        match caller.principal() {
            CallerPrincipal::Person(id) => Self::person(id.clone()),
            CallerPrincipal::Agent(id) => Self::agent(format!("agent:{id}")),
            CallerPrincipal::App(id) => Self::agent(format!("app:{id}")),
        }
    }
}

fn descriptor(action: &PublishedAction) -> ActionDescriptor {
    ActionDescriptor {
        address: ActionAddress::new(&action.resource, &action.action),
        resource_kind: action.resource_kind.clone(),
        resource_label: action.resource_label.clone(),
        resource_sensitivity: action.resource_sensitivity,
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

fn app_open_descriptor(package_id: &str, display_name: &str) -> ActionDescriptor {
    ActionDescriptor {
        address: ActionAddress::new(
            format!("{APPLICATION_RESOURCE_PREFIX}{package_id}"),
            APPLICATION_OPEN_ACTION,
        ),
        resource_kind: "applications.app".into(),
        resource_label: display_name.to_string(),
        resource_sensitivity: tilo_actions::Sensitivity::Public,
        action_label: format!("Open {display_name}"),
        description: Some("Launch this application into the current workspace".into()),
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

/// A workspace application launch that opens published content.
struct ContentLaunch {
    room: String,
    app_id: String,
    content_hash: String,
}

/// `request` as a launch with content, when it is one.
fn content_launch_target(request: &InvokeRequest) -> Option<ContentLaunch> {
    let room = workspace_provider_room(&request.target.provider)?;
    let address = &request.target.address;
    let app_id = address.resource.strip_prefix(APPLICATION_RESOURCE_PREFIX)?;
    if address.action != APPLICATION_OPEN_ACTION {
        return None;
    }
    let content_hash = request.argument.to_slot().filter(|hash| !hash.is_empty())?;
    Some(ContentLaunch {
        room: room.to_string(),
        app_id: app_id.to_string(),
        content_hash,
    })
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
            RemoteActionMessage::CatalogueResponse { .. }
            | RemoteActionMessage::Receipt(_)
            | RemoteActionMessage::Update { .. }
            | RemoteActionMessage::CancelResponse { .. } => Some(person.clone()),
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
    pub(crate) fn app_action_access(
        &self,
        app_id: &str,
    ) -> crate::shell::app_action_access::ActionAccess {
        self.app_action_access
            .read()
            .map(|resolver| resolver(app_id))
            .unwrap_or_default()
    }

    /// Read declared access from `resolver` from now on.
    pub(crate) fn set_app_action_access(
        &self,
        resolver: crate::shell::app_action_access::AccessResolver,
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
    fn workspaces_for(
        &self,
        caller: &Caller,
    ) -> Vec<(crate::shell::state::Workspace, crate::shell::state::Actor)> {
        let Some(membership) = self.membership.as_ref() else {
            return Vec::new();
        };
        let actor = BrokerActor::for_caller(caller);
        let tree = membership.read();
        tree.desktop
            .workspaces
            .iter()
            .filter(|workspace| {
                let room = workspace.room.to_string();
                match caller.room() {
                    Some(confined) => confined == room,
                    None => match caller.principal() {
                        CallerPrincipal::Person(id) => workspace.role_of(id).is_some(),
                        CallerPrincipal::Agent(id) => workspace.role_of_agent(id).is_some(),
                        // An application is always admitted into one room;
                        // unconfined, it is a member of none.
                        CallerPrincipal::App(_) => false,
                    },
                }
            })
            .map(|workspace| (workspace.clone(), actor.policy_in(workspace)))
            .collect()
    }

    pub(super) fn allowed(
        workspace: &crate::shell::state::Workspace,
        policy: &crate::shell::state::Actor,
        app: &str,
        action: &str,
        permission: tilo_actions::PermissionRequirement,
    ) -> bool {
        // An action that asks nothing of a person still has to be granted to
        // an application: it holds no membership that would imply it.
        let acting_app = policy
            .principals
            .iter()
            .any(|principal| matches!(principal, crate::shell::state::Principal::App(_)));
        let verb = permission_verb(permission).or_else(|| acting_app.then_some(Verb::Read));
        verb.is_none_or(|verb| {
            workspace.decide(
                policy,
                &ResourceRef::Action {
                    app: app.to_string(),
                    action: action.to_string(),
                },
                verb,
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
        let keymap = (with_rows && caller.device_public_key().is_none())
            .then(settings_store::hardware::ShortcutConfig::load);
        let actor = BrokerActor::for_caller(caller);
        for (workspace, policy) in self.workspaces_for(caller) {
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
                                &surface.app_id,
                                &action.action,
                                action.contract.permission,
                            ) && (!action.contract.speaks_as_participant()
                                || self.performs_for(surface.peer_id, &actor))
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
            let profile = crate::shell::workspace_settings::permission_profile_resource_with_access(
                &workspace, true,
            );
            rows.extend(
                tilo_actions::door::survey(&[profile])
                    .into_iter()
                    .filter(|descriptor| {
                        Self::allowed(
                            &workspace,
                            &policy,
                            crate::shell::state::WORKSPACE_GOVERNANCE_APP_ID,
                            &descriptor.address.action,
                            descriptor.contract.permission,
                        )
                    })
                    .map(|descriptor| {
                        action_row(
                            workspace_provider(&room),
                            crate::shell::state::WORKSPACE_GOVERNANCE_APP_ID,
                            descriptor,
                        )
                    }),
            );
            // Public-room governance, the same actions the palette offers.
            let public_room = crate::shell::public_room_governance::resource(&workspace);
            rows.extend(
                tilo_actions::door::survey(&[public_room])
                    .into_iter()
                    .filter(|descriptor| {
                        Self::allowed(
                            &workspace,
                            &policy,
                            crate::shell::state::WORKSPACE_GOVERNANCE_APP_ID,
                            &descriptor.address.action,
                            descriptor.contract.permission,
                        )
                    })
                    .map(|descriptor| {
                        action_row(
                            workspace_provider(&room),
                            crate::shell::state::WORKSPACE_GOVERNANCE_APP_ID,
                            descriptor,
                        )
                    }),
            );
            rows.extend(
                automation_host::descriptors(&workspace)
                    .into_iter()
                    .filter(|descriptor| {
                        Self::allowed(
                            &workspace,
                            &policy,
                            SHORTCUTS_APP_ID,
                            &descriptor.address.action,
                            descriptor.contract.permission,
                        )
                    })
                    .map(|descriptor| {
                        action_row(workspace_provider(&room), SHORTCUTS_APP_ID, descriptor)
                    }),
            );
            if let Some(keymap) = &keymap {
                rows.extend(
                    keyboard_shortcuts::catalogue_rows(&workspace_provider(&room), keymap)
                        .into_iter()
                        .filter(|row| {
                            row.action.as_ref().is_some_and(|descriptor| {
                                Self::allowed(
                                    &workspace,
                                    &policy,
                                    keyboard_shortcuts::APP_ID,
                                    &descriptor.address.action,
                                    descriptor.contract.permission,
                                )
                            })
                        }),
                );
            }
            if caller.room().is_some() {
                rows.extend(
                    crate::shell::launcher::gui_apps()
                        .into_iter()
                        .filter(|app| !live_apps.contains(&app.package_id))
                        .filter(|app| {
                            workspace.decide(
                                &policy,
                                &ResourceRef::App(app.package_id.clone()),
                                Verb::Write,
                            ) == CapDecision::Allow
                        })
                        .map(|app| {
                            action_row(
                                workspace_provider(&room),
                                &app.package_id,
                                app_open_descriptor(&app.package_id, &app.display_name),
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
        workspace: &crate::shell::state::Workspace,
        policy: &crate::shell::state::Actor,
        live_apps: &std::collections::HashSet<String>,
        mut rows: Option<&mut Vec<CatalogueRow>>,
        providers: &mut Vec<CatalogueProvider>,
    ) {
        let room = workspace.room.to_string();
        for surface in self.launchable.surfaces().into_iter().filter(|surface| {
            surface.route.cluster_id == room
                && !live_apps.contains(&surface.app_id)
                && workspace.decide(
                    policy,
                    &ResourceRef::App(surface.app_id.clone()),
                    Verb::Write,
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
            .map(|(workspace, _)| workspace.room.to_string())
            .collect::<Vec<_>>();
        self.action_directory_roots()
            .into_iter()
            .find(|root| root.peer_id == peer_id && rooms.contains(&root.cluster_id))
    }

    /// One page of what `caller` is offered. A provider publishing a live
    /// directory is browsed there, on the broker's local set; everything
    /// else is answered at once from the eager surfaces.
    /// Every caller's catalogue goes through here, so this is where labels
    /// are redacted for whoever reads them: a remote device, an agent or an
    /// MCP bridge receives each resource label as its sensitivity allows.
    pub(crate) fn action_query(
        self: &Arc<Self>,
        caller: &Caller,
        query: &CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        let audience = caller.audience();
        self.action_query_unredacted(caller, query)
            .map(move |page| page.map(|page| page.redacted_for(audience)))
    }

    fn action_query_unredacted(
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
        let actor = BrokerActor::for_caller(caller);
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
            queue.try_resolve(
                actor,
                &root.cluster_id,
                root.peer_id,
                revision,
                &target.resource,
                &target.action,
            )
        } else {
            let limits = query.limits.unwrap_or_default();
            queue.try_query(
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
    pub(crate) fn action_invoke(
        self: &Arc<Self>,
        caller: &Caller,
        own_peer: Option<u64>,
        launch: Option<&LaunchScope>,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if is_launchable_provider(&request.target.provider) {
            return self.invoke_launchable(caller, request);
        }
        if let Some(root) = self.directory_target(caller, request) {
            return self.invoke_directory(caller, root, request);
        }
        if let Some(content) = content_launch_target(request) {
            return self.invoke_content_launch(caller, launch.cloned(), content, request);
        }
        if let Some(room) = workspace_provider_room(&request.target.provider)
            && let Some(shortcut) = automation_host::run_target(&request.target.address)
        {
            return Pending::ready(self.invoke_shortcut_run(caller, room, shortcut, request));
        }
        Pending::ready(self.invoke_eager(caller, own_peer, request))
    }

    /// Launch an application with published content. Staging the content
    /// reads the blob and writes a file, so it runs on the I/O pool and the
    /// admission arrives once the launch has happened, never blocking the
    /// caller's thread. Authority is checked before storage is touched and
    /// again after staging, so the launch acts on current authority.
    fn invoke_content_launch(
        self: &Arc<Self>,
        caller: &Caller,
        scope: Option<LaunchScope>,
        launch: ContentLaunch,
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
        let admitted = self
            .authorized_launch(caller, &launch.room, &launch.app_id)
            .and_then(|_| {
                super::validate_launch_content_hash(&launch.content_hash).map_err(refusal)
            })
            .and_then(|()| {
                // Content is only ever read through the asking application's
                // own scope; a caller without one cannot open it by hash.
                scope.as_ref().map(|_| ()).ok_or_else(|| {
                    refusal(
                        "launch content requires an application the launcher authenticated".into(),
                    )
                })
            })
            .and_then(|()| {
                crate::session_runtime::runtime()
                    .map_err(|reason| ActionHostError::Unavailable { reason })
            });
        let runtime = match admitted {
            Ok(runtime) => runtime,
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
        let invocation = request.invocation;
        runtime.spawn(async move {
            // The content is read through the scope of the application that
            // asked, never with the shell's own authority.
            let staged = super::stage_launch_content(
                &launch.content_hash,
                scope.as_ref(),
                Arc::new(SessionContentStore),
            )
            .await;
            // A caller that stopped waiting has been told the launch did not
            // start; keep it so.
            let started = if completer.is_abandoned() {
                Err(ActionHostError::Unavailable {
                    reason: "the caller stopped waiting for admission".into(),
                })
            } else {
                staged.map_err(refusal).and_then(|staged| {
                    let (mut workspace, _) =
                        state.authorized_launch(&caller, &launch.room, &launch.app_id)?;
                    let document =
                        staged.map(|staged| staged.grant_to(&mut workspace, &launch.app_id));
                    Ok(state.launch_into(
                        &workspace,
                        &BrokerActor::for_caller(&caller),
                        &launch.app_id,
                        document.as_deref(),
                    ))
                })
            };
            let result = match state.remote_invocations.lock() {
                Ok(mut ledger) => match started {
                    Ok(job) => {
                        ledger.complete(&invocation, job);
                        drop(ledger);
                        Ok(state.receipt_for(invocation, job))
                    }
                    Err(error) => {
                        ledger.abandon(&invocation);
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
        let admitted = self.admit_launchable(caller, request).and_then(|prepared| {
            crate::session_runtime::runtime()
                .map(|runtime| (prepared, runtime))
                .map_err(|reason| ActionHostError::Unavailable { reason })
        });
        let ((workspace, app_id), runtime) = match admitted {
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
        runtime.spawn(async move {
            let started = state
                .relaunch_and_deliver(&completer, &caller, &workspace, &app_id, &request)
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
    /// launch the app and use the retained action, and carries the
    /// confirmation its contract asks for, so a refusal never costs a launch.
    /// Returns the workspace to launch into and the application.
    fn admit_launchable(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(crate::shell::state::Workspace, String), ActionHostError> {
        let retained = self
            .launchable
            .surface_for(&request.target.provider)
            .ok_or(ActionHostError::UnknownAction)?;
        let (workspace, policy) =
            self.authorized_launch(caller, &retained.route.cluster_id, &retained.app_id)?;
        let address = &request.target.address;
        let action = retained
            .actions
            .iter()
            .find(|action| action.resource == address.resource && action.action == address.action)
            .filter(|action| {
                Self::allowed(
                    &workspace,
                    &policy,
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
    async fn relaunch_and_deliver(
        &self,
        completer: &Completer<Result<InvocationReceipt, ActionHostError>>,
        caller: &Caller,
        workspace: &crate::shell::state::Workspace,
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
                return self.start_for(caller, None, &delivered);
            }
            if !launched {
                (self.launchable.launcher())(app_id, workspace).map_err(|reason| {
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
            tokio::time::sleep(POLL).await;
        }
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

    /// The hosted workspace `room` and the caller's policy actor in it, when
    /// `caller` may launch `app_id` there.
    fn authorized_launch(
        &self,
        caller: &Caller,
        room: &str,
        app_id: &str,
    ) -> Result<(crate::shell::state::Workspace, crate::shell::state::Actor), ActionHostError> {
        if caller.room().is_none() {
            return Err(ActionHostError::UnknownAction);
        }
        let (workspace, policy) = self
            .workspaces_for(caller)
            .into_iter()
            .find(|(workspace, _)| workspace.room.to_string() == room)
            .ok_or(ActionHostError::UnknownAction)?;
        if workspace.decide(&policy, &ResourceRef::App(app_id.to_string()), Verb::Write)
            != CapDecision::Allow
        {
            return Err(refusal(
                "the caller is not allowed to launch that application".into(),
            ));
        }
        Ok((workspace, policy))
    }

    fn invoke_eager(
        &self,
        caller: &Caller,
        own_peer: Option<u64>,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        let mut ledger =
            self.remote_invocations
                .lock()
                .map_err(|_| ActionHostError::Unavailable {
                    reason: "the invocation ledger is unavailable".into(),
                })?;
        let (job, _) = ledger.admit(caller, request, || {
            self.start_for(caller, own_peer, request)
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
        let resolved = self.action_directory_request_queue().try_resolve(
            BrokerActor::for_caller(caller),
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
            ))) => return Err(ActionHostError::StaleRevision),
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
        let actor = BrokerActor::for_caller(caller);
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
            argument,
            &actor,
            token.as_deref(),
            request.context.clone(),
        )
        .map(|job| job.id)
        .map_err(refusal)
    }

    fn start_for(
        &self,
        caller: &Caller,
        own_peer: Option<u64>,
        request: &InvokeRequest,
    ) -> Result<u64, ActionHostError> {
        let actor = BrokerActor::for_caller(caller);
        let address = &request.target.address;
        let argument = request.argument.to_slot();
        let provider = request.target.provider.as_str();
        if let Some(room) = workspace_provider_room(provider) {
            return self.start_workspace_action(caller, &actor, room, request, argument.as_deref());
        }
        let peer_id = if provider == OWN_PROVIDER {
            own_peer.ok_or(ActionHostError::UnknownAction)?
        } else {
            provider
                .parse::<u64>()
                .map_err(|_| ActionHostError::UnknownAction)?
        };
        let offered = self
            .published_surfaces()
            .into_iter()
            .find(|surface| surface.peer_id == peer_id && surface.live)
            .and_then(|surface| {
                let (workspace, policy) =
                    self.workspaces_for(caller)
                        .into_iter()
                        .find(|(workspace, _)| {
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
                    &surface.app_id,
                    &action.action,
                    action.contract.permission,
                )
                .then_some(action)
            })
            .ok_or(ActionHostError::UnknownAction)?;
        let token = self.confirmation_token(
            peer_id,
            &offered,
            argument.as_deref(),
            &actor.id,
            request.confirmation.as_ref(),
        )?;
        self.start_invocation_confirmed_for(
            peer_id,
            &address.resource,
            &address.action,
            argument.as_deref(),
            &actor,
            token.as_deref(),
            request.context.clone(),
        )
        .map(|job| job.id)
        .map_err(refusal)
    }

    fn start_workspace_action(
        &self,
        caller: &Caller,
        actor: &BrokerActor,
        room: &str,
        request: &InvokeRequest,
        argument: Option<&str>,
    ) -> Result<u64, ActionHostError> {
        let address = &request.target.address;
        if !self
            .workspaces_for(caller)
            .iter()
            .any(|(workspace, _)| workspace.room.to_string() == room)
        {
            return Err(ActionHostError::UnknownAction);
        }
        if let Some(app_id) = address.resource.strip_prefix(APPLICATION_RESOURCE_PREFIX)
            && address.action == APPLICATION_OPEN_ACTION
        {
            // A launch with content is staged first; see
            // `invoke_content_launch`.
            if argument.is_some_and(|content| !content.is_empty()) {
                return Err(ActionHostError::InvalidRequest {
                    reason: "a launch with content is staged before it is admitted".into(),
                });
            }
            let (workspace, _) = self.authorized_launch(caller, room, app_id)?;
            return Ok(self.launch_into(&workspace, actor, app_id, None));
        }
        if address.resource == tilo_actions::remote::ROOM_RECORDS_RESOURCE {
            return self.invoke_room_records(
                caller,
                actor,
                room,
                &address.action,
                &request.argument,
            );
        }
        if address.resource == tilo_actions::remote::SESSION_REPLAY_RESOURCE
            && address.action == tilo_actions::remote::SESSION_REPLAY_ACTION
        {
            return self.replay_session(caller, actor, room, &request.argument);
        }
        if address.resource == tilo_actions::remote::PEOPLE_RESOURCE {
            return self.request_person(caller, actor, room, &address.action, argument);
        }
        if address.resource == tilo_actions::remote::RESOURCES_RESOURCE
            && address.action == tilo_actions::remote::RESOURCE_OPEN_ACTION
        {
            return self.open_published_resource(caller, actor, room, argument, request);
        }
        if address.resource == automation_host::LIBRARY_RESOURCE {
            return self.start_shortcut_library(caller, actor, room, &address.action, argument);
        }
        if keyboard_shortcuts::is_resource(&address.resource)
            && caller.device_public_key().is_none()
        {
            return self.start_keyboard_shortcut_action(caller, actor, room, address, argument);
        }
        if address.resource == crate::shell::public_room_governance::resource_id(room) {
            return self
                .invoke_public_room_action(
                    room,
                    actor,
                    &address.action,
                    argument,
                    &confirmation_evidence(request.confirmation.as_ref()),
                )
                .map(|(id, _)| id)
                .map_err(refusal);
        }
        if crate::shell::workspace_settings::is_governance_action(&address.action) {
            return self
                .invoke_workspace_profile_action(
                    room,
                    actor,
                    &address.resource,
                    &address.action,
                    argument,
                    &confirmation_evidence(request.confirmation.as_ref()),
                )
                .map(|(id, _)| id)
                .map_err(refusal);
        }
        Err(ActionHostError::UnknownAction)
    }

    /// The room's record index as a resource: add or remove a reference —
    /// the sharing verb of `rooms.md`. A Commenter may share what they post,
    /// so it needs `Comment` on the room; no bytes move either way. The host
    /// folds the request into the room's index on its next frame, the same
    /// pass that folds in a peer's references.
    fn invoke_room_records(
        &self,
        caller: &Caller,
        actor: &BrokerActor,
        room: &str,
        action: &str,
        argument: &tilo_actions::remote::InvokeArgument,
    ) -> Result<u64, ActionHostError> {
        let reference = match action {
            tilo_actions::remote::ROOM_RECORD_REFERENCE_ACTION => true,
            tilo_actions::remote::ROOM_RECORD_UNREFERENCE_ACTION => false,
            _ => return Err(ActionHostError::UnknownAction),
        };
        let tilo_actions::remote::InvokeArgument::Json(arguments) = argument else {
            return Err(ActionHostError::InvalidRequest {
                reason: "a room reference names its record as {\"id\", \"kind\"}".into(),
            });
        };
        let id = arguments
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| ui::record::RecordId::parse(id).is_some())
            .ok_or_else(|| ActionHostError::InvalidRequest {
                reason: "the record id is missing or is not one this build can read".into(),
            })?;
        let kind = arguments
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .filter(|kind| ui::record::RecordKind::new(*kind).is_valid());
        if reference && kind.is_none() {
            return Err(ActionHostError::InvalidRequest {
                reason: "a reference names the record's kind".into(),
            });
        }
        let (workspace, policy) = self
            .workspaces_for(caller)
            .into_iter()
            .find(|(workspace, _)| workspace.room.to_string() == room)
            .ok_or(ActionHostError::UnknownAction)?;
        if workspace.decide(&policy, &ResourceRef::Workspace, Verb::Comment) != CapDecision::Allow {
            return Err(ActionHostError::NotPermitted);
        }
        let Some(membership) = self.membership.as_ref() else {
            return Err(ActionHostError::Unavailable {
                reason: "this host keeps no room index".into(),
            });
        };
        {
            let mut tree = membership.write();
            if !reference && !tree.may_withdraw_room_record(workspace.room, id) {
                return Err(ActionHostError::Refused {
                    reason: "another member brought this record in; only they or the room's administrator can take it out".into(),
                });
            }
            if reference {
                tree.request_room_reference(
                    workspace.room,
                    id.to_string(),
                    kind.unwrap_or_default().to_string(),
                );
            } else {
                tree.request_room_unreference(workspace.room, id.to_string());
            }
        }
        let job = self
            .invocation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(job)));
        self.record_completion(
            job,
            actor.id.clone(),
            tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
        );
        Ok(job)
    }

    /// A person's card: open the room the caller shares with them, or invite
    /// them into one of the caller's workspaces. The host answers on its next
    /// frame through the direct-room and invite flows, which check the
    /// caller's authority themselves (an invite needs a workspace the caller
    /// governs with `Invite`) and say so when there is none.
    fn request_person(
        &self,
        caller: &Caller,
        actor: &BrokerActor,
        room: &str,
        action: &str,
        argument: Option<&str>,
    ) -> Result<u64, ActionHostError> {
        let person = argument
            .filter(|person| person.len() == 64 && person.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| ActionHostError::InvalidRequest {
                reason: "name the person by their participant id".into(),
            })?
            .to_ascii_lowercase();
        if person == actor.id {
            return Err(ActionHostError::InvalidRequest {
                reason: "that is you".into(),
            });
        }
        let request = match action {
            tilo_actions::remote::PERSON_MESSAGE_ACTION => {
                tilo_desktop_model::tree::PersonRequest::Message { person }
            }
            tilo_actions::remote::PERSON_INVITE_ACTION => {
                tilo_desktop_model::tree::PersonRequest::Invite { person }
            }
            _ => return Err(ActionHostError::UnknownAction),
        };
        if actor.is_agent {
            return Err(ActionHostError::NotPermitted);
        }
        let (workspace, policy) = self
            .workspaces_for(caller)
            .into_iter()
            .find(|(workspace, _)| workspace.room.to_string() == room)
            .ok_or(ActionHostError::UnknownAction)?;
        if workspace.decide(&policy, &ResourceRef::Workspace, Verb::Read) != CapDecision::Allow {
            return Err(ActionHostError::NotPermitted);
        }
        let Some(membership) = self.membership.as_ref() else {
            return Err(ActionHostError::Unavailable {
                reason: "this host has no desktop to answer on".into(),
            });
        };
        membership.write().request_person(request);
        let job = self
            .invocation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(job)));
        self.record_completion(
            job,
            actor.id.clone(),
            tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
        );
        Ok(job)
    }

    /// Replay a `session` record for the person asking: the shell opens an
    /// ephemeral replay room and follows the recording there. Reading the
    /// room is enough, as for anything else posted in it.
    fn replay_session(
        &self,
        caller: &Caller,
        actor: &BrokerActor,
        room: &str,
        argument: &tilo_actions::remote::InvokeArgument,
    ) -> Result<u64, ActionHostError> {
        let tilo_actions::remote::InvokeArgument::Text(id) = argument else {
            return Err(ActionHostError::InvalidRequest {
                reason: "name the session record to replay".into(),
            });
        };
        let record = ui::record::RecordId::parse(id.trim()).ok_or_else(|| {
            ActionHostError::InvalidRequest {
                reason: "the record id is not one this build can read".into(),
            }
        })?;
        let (workspace, policy) = self
            .workspaces_for(caller)
            .into_iter()
            .find(|(workspace, _)| workspace.room.to_string() == room)
            .ok_or(ActionHostError::UnknownAction)?;
        if workspace.decide(&policy, &ResourceRef::Workspace, Verb::Read) != CapDecision::Allow {
            return Err(ActionHostError::NotPermitted);
        }
        if actor.is_agent {
            return Err(ActionHostError::Refused {
                reason: "a replay is opened for a person watching it".into(),
            });
        }
        let replay = self.replay().ok_or_else(|| ActionHostError::Unavailable {
            reason: "this session cannot replay recordings".into(),
        })?;
        let replay_room = replay
            .open_record(
                &record,
                &actor.id,
                crate::shell::replay::Presentation::Shown,
            )
            .map_err(|error| ActionHostError::Refused {
                reason: error.to_string(),
            })?;
        let job = self
            .invocation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(job)));
        self.record_completion(
            job,
            actor.id.clone(),
            tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput {
                    value: Some(match replay_room {
                        Some(room) => serde_json::json!({ "room": room.to_string() }),
                        None => serde_json::json!({ "fetching": true }),
                    }),
                    resources: Vec::new(),
                },
            },
        );
        Ok(job)
    }

    /// Open a resource another app in this room published, by its stable
    /// id: the workspace invokes that resource's own `open`, as the caller
    /// and under the caller's permission, exactly as if they had chosen it
    /// in the palette. An item no open app publishes is refused in words.
    fn open_published_resource(
        &self,
        caller: &Caller,
        actor: &BrokerActor,
        room: &str,
        argument: Option<&str>,
        request: &InvokeRequest,
    ) -> Result<u64, ActionHostError> {
        let resource = argument
            .filter(|id| !id.is_empty() && id.len() <= 1024)
            .ok_or_else(|| ActionHostError::InvalidRequest {
                reason: "name the resource to open".into(),
            })?;
        let (workspace, policy) = self
            .workspaces_for(caller)
            .into_iter()
            .find(|(workspace, _)| workspace.room.to_string() == room)
            .ok_or(ActionHostError::UnknownAction)?;
        let (peer_id, action) = self
            .published_surfaces()
            .into_iter()
            .filter(|surface| surface.live && surface.route.cluster_id == room)
            .find_map(|surface| {
                let action = surface.actions.iter().find(|action| {
                    action.resource == resource
                        && action.action == tilo_actions::remote::RESOURCE_OPEN_ACTION
                })?;
                Self::allowed(
                    &workspace,
                    &policy,
                    &surface.app_id,
                    &action.action,
                    action.contract.permission,
                )
                .then(|| (surface.peer_id, action.clone()))
            })
            .ok_or_else(|| ActionHostError::Refused {
                reason: "no open app in this workspace publishes that item".into(),
            })?;
        self.start_invocation_confirmed_for(
            peer_id,
            &action.resource,
            &action.action,
            None,
            actor,
            None,
            request.context.clone(),
        )
        .map(|job| job.id)
        .map_err(refusal)
    }

    /// Apply one shortcut library action for `caller` in `room`. A saved
    /// shortcut is checked against exactly what `caller` is offered now.
    fn start_shortcut_library(
        &self,
        caller: &Caller,
        actor: &BrokerActor,
        room: &str,
        action: &str,
        argument: Option<&str>,
    ) -> Result<u64, ActionHostError> {
        let descriptor =
            automation_host::library_descriptor(action).ok_or(ActionHostError::UnknownAction)?;
        let (workspace, policy) = self
            .workspaces_for(caller)
            .into_iter()
            .find(|(workspace, _)| workspace.room.to_string() == room)
            .ok_or(ActionHostError::UnknownAction)?;
        if !Self::allowed(
            &workspace,
            &policy,
            SHORTCUTS_APP_ID,
            action,
            descriptor.contract.permission,
        ) {
            return Err(ActionHostError::NotPermitted);
        }
        let operation = automation_host::decode_library(action, argument)
            .map_err(|reason| ActionHostError::InvalidRequest { reason })?;
        let catalog = tilo_automation::ActionCatalog::new(self.eager_catalogue(caller, true).0);
        let membership = self
            .membership
            .as_ref()
            .ok_or_else(|| ActionHostError::Unavailable {
                reason: "the action broker has no workspace authority".into(),
            })?;
        let id = self
            .invocation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let keymap = settings_store::hardware::ShortcutConfig::load();
        let output = {
            let mut tree = membership.write();
            let output = automation_host::apply_library(
                &mut tree,
                workspace.id,
                operation,
                &automation_host::SaveContext {
                    catalog: &catalog,
                    saver: &actor.id,
                    keymap: &keymap,
                },
            )
            .map_err(|reason| ActionHostError::Refused { reason })?;
            if action != automation_host::LIST_ACTION {
                let _ = tree.append_workspace_effect_receipt(
                    workspace.id,
                    invocation,
                    &actor.id,
                    automation_host::library_context(&descriptor),
                    tilo_actions::EffectReceiptStatus::Succeeded,
                );
            }
            output
        };
        self.record_completion(
            id,
            actor.id.clone(),
            tilo_actions::InvocationUpdate::Succeeded { invocation, output },
        );
        Ok(id)
    }

    /// List or change the desktop's keyboard shortcuts for `caller` in
    /// `room`. Changing one takes the same Settings grant as changing any
    /// other setting; a clash fails naming what holds the chord.
    fn start_keyboard_shortcut_action(
        &self,
        caller: &Caller,
        actor: &BrokerActor,
        room: &str,
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Result<u64, ActionHostError> {
        let mut keymap = settings_store::hardware::ShortcutConfig::load();
        let descriptor = keyboard_shortcuts::descriptor_at(&keymap, address)
            .ok_or(ActionHostError::UnknownAction)?;
        let (workspace, policy) = self
            .workspaces_for(caller)
            .into_iter()
            .find(|(workspace, _)| workspace.room.to_string() == room)
            .ok_or(ActionHostError::UnknownAction)?;
        if !Self::allowed(
            &workspace,
            &policy,
            keyboard_shortcuts::APP_ID,
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
            keyboard_shortcuts::decode(&keymap, address, argument).and_then(|operation| {
                let output = keyboard_shortcuts::apply(&mut keymap, &operation)?;
                if operation.changes() {
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
            actor.id.clone(),
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
                automation_host::RunStart {
                    run: tilo_automation::RunId(request.invocation.0),
                    trigger: tilo_automation::RunTrigger::Manual,
                    mode: automation_host::run_mode(request.confirmation.as_ref()),
                    inputs,
                },
                automation_host::DoorHooks::new(request.confirmation.clone()),
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
        let hooks = automation_host::SheetHooks::new(self.step_prompts.clone(), &shortcut.name);
        self.start_shortcut_run(
            caller,
            room,
            shortcut,
            automation_host::RunStart {
                run: tilo_automation::RunId::new(),
                trigger: tilo_automation::RunTrigger::Manual,
                mode: tilo_automation::RunMode::Interactive,
                inputs,
            },
            hooks,
        )
    }

    /// The oldest step of an interactive run waiting for its person.
    pub(crate) fn take_step_prompt(&self) -> Option<automation_host::PromptedStep> {
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
    pub(crate) fn start_triggered_shortcut_run(
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
            .find(|(workspace, _)| workspace.room.to_string() == room)
            .map(|(workspace, _)| workspace.shortcuts)
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
            let hooks = automation_host::SheetHooks::new(self.step_prompts.clone(), &shortcut.name);
            return self.start_shortcut_run(
                caller,
                room,
                shortcut,
                automation_host::RunStart {
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
            automation_host::RunStart {
                run,
                trigger: tilo_automation::RunTrigger::Fired(trigger.clone()),
                mode: tilo_automation::RunMode::Unattended,
                inputs,
            },
            automation_host::DoorHooks::new(None),
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
            .find(|(workspace, _)| workspace.room.to_string() == room)
            .and_then(|(workspace, _)| {
                workspace
                    .shortcuts
                    .iter()
                    .find(|shortcut| shortcut.id == shortcut_id)
                    .cloned()
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
        start: automation_host::RunStart,
        hooks: K,
    ) -> Result<u64, ActionHostError>
    where
        K: tilo_automation::RunHooks + Send + 'static,
    {
        let runtime = crate::session_runtime::runtime()
            .map_err(|reason| ActionHostError::Unavailable { reason })?;
        let actor = BrokerActor::for_caller(caller).id;
        let job = self
            .invocation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(job)));
        self.record_completion(
            job,
            actor.clone(),
            tilo_actions::InvocationUpdate::Accepted { invocation },
        );
        let host = crate::shell::remote_actions::BrokerActionHost::new(
            crate::shell::collab_broker::AppActionBroker::from_state(Arc::clone(self)),
        );
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
                actor.clone(),
                automation_host::completion(invocation, &report),
            );
            state
                .shortcut_runs
                .push(automation_host::FinishedShortcutRun {
                    room,
                    shortcut,
                    invoker: actor,
                    report,
                });
        });
        Ok(job)
    }

    /// Shortcut runs finished since the last call, oldest first.
    pub(crate) fn take_finished_shortcut_runs(&self) -> Vec<automation_host::FinishedShortcutRun> {
        self.shortcut_runs.take()
    }

    /// Launch `app_id` into `workspace`, optionally opening a staged
    /// document, and record the terminal outcome as an invocation.
    fn launch_into(
        &self,
        workspace: &crate::shell::state::Workspace,
        actor: &BrokerActor,
        app_id: &str,
        document: Option<&std::path::Path>,
    ) -> u64 {
        let id = self
            .invocation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let update =
            match crate::shell::launcher::launch_document_in_workspace(app_id, workspace, document)
            {
                Ok(_) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err(error) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "launch_failed".into(),
                        message: error.to_string(),
                        details: None,
                        retryable: false,
                    },
                },
            };
        self.record_completion(id, actor.id.clone(), update);
        id
    }

    pub(crate) fn remote_job(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<u64> {
        self.remote_invocations
            .lock()
            .ok()
            .and_then(|ledger| ledger.get(caller, id).copied())
    }

    /// The latest state of an invocation `caller` admitted.
    pub(crate) fn action_observe(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        let job = self.remote_job(caller, id)?;
        self.invocation_status(job)
            .map(|update| InvocationState::new(*id, update))
    }

    /// Ask an invocation `caller` admitted to stop.
    pub(crate) fn action_cancel(
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
            sensitivity: action.resource_sensitivity,
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
            // Validated on arrival; an unreadable declaration is treated as
            // the most restrictive one rather than as public.
            sensitivity: tilo_actions::Sensitivity::from_wire(&row.resource_sensitivity)
                .unwrap_or(tilo_actions::Sensitivity::Secret),
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
    }
}

fn bridge_error(error: ActionDirectoryBridgeError) -> ActionHostError {
    match error {
        ActionDirectoryBridgeError::InvalidRequest => ActionHostError::InvalidRequest {
            reason: "the directory request is malformed".into(),
        },
        other => ActionHostError::Unavailable {
            reason: format!("the action directory is {other:?}"),
        },
    }
}
