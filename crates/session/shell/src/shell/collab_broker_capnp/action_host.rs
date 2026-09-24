//! The broker's answers to the remote-action vocabulary.
//!
//! Every caller that is not the application itself — a peer on the shell
//! network, an app or agent on this machine, the MCP bridge, the palette —
//! reaches published actions through these methods, wrapped as an
//! [`ActionHost`](tilo_actions::remote::ActionHost) by
//! [`BrokerActionHost`](crate::shell::remote_actions::BrokerActionHost).
//! Authority is decided from the transport-supplied [`Caller`] alone, with
//! the same capability checks the admission path has always applied.

use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, ActionHostError, Caller, CallerPrincipal,
    Catalogue, CatalogueEntry, Confirmation, InvocationReceipt, InvocationState, InvokeRequest,
    OWN_PROVIDER, ReceiptStatus, workspace_provider, workspace_provider_room,
};
use tilo_actions::{ActionAddress, ActionDescriptor, Collects, ExecutionLocus, InputSpec};

use super::{
    BrokerActor, CapDecision, CapnpBrokerState, PublishedAction, PublishedInput, ResourceRef, Verb,
    permission_verb,
};

impl BrokerActor {
    /// The broker actor a caller is admitted as.
    pub(crate) fn for_caller(caller: &Caller) -> Self {
        match caller.principal() {
            CallerPrincipal::Person(id) => Self::person(id.clone()),
            CallerPrincipal::Agent(id) => Self::agent(format!("agent:{id}")),
        }
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
        action_label: format!("Open {display_name}"),
        description: Some("Launch this application into the current workspace".into()),
        locus: ExecutionLocus::default(),
        kind: tilo_actions::OperationKind::Effect,
        contract: tilo_actions::ActionContract::default(),
        // An invocation may carry the content hash of bytes to open
        // read-only as its text argument; without one the app opens empty.
        input: InputSpec::None,
        params: Vec::new(),
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
        tree.workspaces
            .iter()
            .filter(|workspace| {
                let room = workspace.room.to_string();
                match caller.room() {
                    Some(confined) => confined == room,
                    None => match caller.principal() {
                        CallerPrincipal::Person(id) => workspace.role_of(id).is_some(),
                        CallerPrincipal::Agent(id) => workspace.role_of_agent(id).is_some(),
                    },
                }
            })
            .map(|workspace| {
                let policy = if actor.is_agent {
                    workspace.actor_for_agent(actor.id.strip_prefix("agent:").unwrap_or(&actor.id))
                } else {
                    workspace.actor_for_person(&actor.id)
                };
                (workspace.clone(), policy)
            })
            .collect()
    }

    fn allowed(
        workspace: &crate::shell::state::Workspace,
        policy: &crate::shell::state::Actor,
        app: &str,
        action: &str,
        permission: tilo_actions::PermissionRequirement,
    ) -> bool {
        permission_verb(permission).is_none_or(|verb| {
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

    /// Everything `caller` is offered: live application actions its role may
    /// invoke, the workspace's own governance actions, and — for a caller on
    /// this machine — launching an application it may open. An action the
    /// caller may not use is absent.
    pub(crate) fn action_catalogue_for(&self, caller: &Caller) -> Catalogue {
        let surfaces = self.published_surfaces();
        let mut entries = Vec::new();
        for (workspace, policy) in self.workspaces_for(caller) {
            let room = workspace.room.to_string();
            let mut live_apps = std::collections::HashSet::new();
            for surface in surfaces
                .iter()
                .filter(|surface| surface.live && surface.route.cluster_id == room)
            {
                live_apps.insert(surface.app_id.clone());
                entries.extend(
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
                            )
                        })
                        .map(|action| CatalogueEntry {
                            provider: surface.peer_id.to_string(),
                            app_id: surface.app_id.clone(),
                            descriptor: descriptor(action),
                        }),
                );
            }
            let profile = crate::shell::workspace_settings::permission_profile_resource_with_access(
                &workspace, true,
            );
            entries.extend(
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
                    .map(|descriptor| CatalogueEntry {
                        provider: workspace_provider(&room),
                        app_id: crate::shell::state::WORKSPACE_GOVERNANCE_APP_ID.into(),
                        descriptor,
                    }),
            );
            if caller.room().is_some() {
                entries.extend(
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
                        .map(|app| CatalogueEntry {
                            provider: workspace_provider(&room),
                            app_id: app.package_id.clone(),
                            descriptor: app_open_descriptor(&app.package_id, &app.display_name),
                        }),
                );
            }
        }
        Catalogue {
            revision: self.published_surfaces_generation().to_string(),
            entries,
        }
    }

    /// Admit `request` for `caller`, or report the invocation already
    /// admitted under its id. `own_peer` resolves [`OWN_PROVIDER`] for a
    /// caller that is itself an application connection.
    pub(crate) fn action_invoke(
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
        Ok(self.invocation_status(job).map_or(
            InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Accepted,
            },
            |update| InvocationState::new(request.invocation, update).receipt(),
        ))
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
        let token = match (
            &offered.contract.confirmation,
            request.confirmation.as_ref(),
        ) {
            (tilo_actions::ConfirmationPolicy::None, _) => None,
            (tilo_actions::ConfirmationPolicy::Confirm, Some(Confirmation::Confirmed)) => Some(
                self.prepare_confirmation(
                    peer_id,
                    &address.resource,
                    &address.action,
                    argument.as_deref(),
                    &actor.id,
                    None,
                )
                .map_err(refusal)?,
            ),
            (
                tilo_actions::ConfirmationPolicy::Phrase { .. },
                Some(Confirmation::Phrase(phrase)),
            ) => Some(
                self.prepare_confirmation(
                    peer_id,
                    &address.resource,
                    &address.action,
                    argument.as_deref(),
                    &actor.id,
                    Some(phrase),
                )
                .map_err(refusal)?,
            ),
            _ => return Err(ActionHostError::ConfirmationRequired),
        };
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
        let Some((workspace, policy)) = self
            .workspaces_for(caller)
            .into_iter()
            .find(|(workspace, _)| workspace.room.to_string() == room)
        else {
            return Err(ActionHostError::UnknownAction);
        };
        if let Some(app_id) = address.resource.strip_prefix(APPLICATION_RESOURCE_PREFIX)
            && address.action == APPLICATION_OPEN_ACTION
        {
            if caller.room().is_none() {
                return Err(ActionHostError::UnknownAction);
            }
            return self
                .launch_application(workspace, &policy, actor, app_id, argument.unwrap_or(""))
                .map_err(refusal);
        }
        if matches!(
            address.action.as_str(),
            crate::shell::state::PROFILE_SAVE_ACTION_ID
                | crate::shell::state::PROFILE_ASSIGN_ACTION_ID
        ) {
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

    /// Launch `app_id` into `workspace`, optionally opening content-addressed
    /// bytes read-only, and record the terminal outcome as an invocation.
    pub(crate) fn launch_application(
        &self,
        mut workspace: crate::shell::state::Workspace,
        policy: &crate::shell::state::Actor,
        actor: &BrokerActor,
        app_id: &str,
        content_hash: &str,
    ) -> Result<u64, String> {
        if workspace.decide(policy, &ResourceRef::App(app_id.to_string()), Verb::Write)
            != CapDecision::Allow
        {
            return Err("the caller is not allowed to launch that application".into());
        }
        let document = super::stage_launch_content(app_id, content_hash, &mut workspace)?;
        let id = self
            .invocation_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(id)));
        let update = match crate::shell::launcher::launch_document_in_workspace(
            app_id,
            &workspace,
            document.as_deref(),
        ) {
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
        Ok(id)
    }

    fn remote_job(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<u64> {
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

    /// Admit `request` for `caller` and return the broker job behind it as
    /// well as the receipt — for an in-process caller that tracks jobs
    /// itself.
    pub(crate) fn action_invoke_job(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(u64, InvocationReceipt), ActionHostError> {
        let receipt = self.action_invoke(caller, None, request)?;
        let job = self
            .remote_job(caller, &request.invocation)
            .ok_or(ActionHostError::UnknownInvocation)?;
        Ok((job, receipt))
    }

    /// Admit `request` against a provider's live, revision-bound action
    /// directory: the owner re-resolves the target at `request.revision`
    /// on the broker's local set before anything starts. Idempotent per
    /// invocation id through the same ledger as every other path.
    pub(crate) async fn action_invoke_resolved(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<(u64, InvocationReceipt), ActionHostError> {
        use tilo_actions::remote::Reservation;
        let room = caller.room().ok_or(ActionHostError::UnknownAction)?;
        let revision =
            request
                .revision
                .as_deref()
                .ok_or_else(|| ActionHostError::InvalidRequest {
                    reason: "a directory action needs the revision it was chosen at".into(),
                })?;
        let peer_id = request
            .target
            .provider
            .parse::<u64>()
            .map_err(|_| ActionHostError::UnknownAction)?;
        let reservation = self
            .remote_invocations
            .lock()
            .map_err(|_| ActionHostError::Unavailable {
                reason: "the invocation ledger is unavailable".into(),
            })?
            .reserve(caller, request)?;
        if reservation == Reservation::Admitted {
            let job = self
                .remote_job(caller, &request.invocation)
                .ok_or(ActionHostError::UnknownInvocation)?;
            let receipt = self.invocation_status(job).map_or(
                InvocationReceipt {
                    invocation: request.invocation,
                    status: ReceiptStatus::Accepted,
                },
                |update| InvocationState::new(request.invocation, update).receipt(),
            );
            return Ok((job, receipt));
        }
        let started = self
            .start_directory_action(caller, room, peer_id, revision, request)
            .await;
        let mut ledger =
            self.remote_invocations
                .lock()
                .map_err(|_| ActionHostError::Unavailable {
                    reason: "the invocation ledger is unavailable".into(),
                })?;
        match started {
            Ok(started) => {
                ledger.complete(&request.invocation, started.invocation_id);
                drop(ledger);
                Ok((
                    started.invocation_id,
                    InvocationState::new(request.invocation, started.initial_update).receipt(),
                ))
            }
            Err(error) => {
                ledger.abandon(&request.invocation);
                drop(ledger);
                Err(error)
            }
        }
    }

    async fn start_directory_action(
        &self,
        caller: &Caller,
        room: &str,
        peer_id: u64,
        revision: &str,
        request: &InvokeRequest,
    ) -> Result<super::StartedDirectoryAction, ActionHostError> {
        use super::{ActionDirectoryBridgeError, ActionDirectoryBridgeResponse};
        let confirmation = request
            .confirmation
            .clone()
            .map(|confirmation| match confirmation {
                Confirmation::Confirmed => super::DirectoryActionConfirmation::Confirmed,
                Confirmation::Phrase(phrase) => super::DirectoryActionConfirmation::Phrase(phrase),
            });
        let bridge_error = |error: ActionDirectoryBridgeError| match error {
            ActionDirectoryBridgeError::NotFound => ActionHostError::UnknownAction,
            ActionDirectoryBridgeError::ConfirmationRequired => {
                ActionHostError::ConfirmationRequired
            }
            ActionDirectoryBridgeError::StaleRevision => ActionHostError::InvalidRequest {
                reason: "the action changed since that revision; list it again".into(),
            },
            ActionDirectoryBridgeError::InvalidRequest => ActionHostError::InvalidRequest {
                reason: "the directory request is malformed".into(),
            },
            other => ActionHostError::Unavailable {
                reason: format!("the action directory is {other:?}"),
            },
        };
        let reply = self
            .action_directory_request_queue()
            .try_start_resolved(
                BrokerActor::for_caller(caller),
                room,
                peer_id,
                revision,
                &request.target.address.resource,
                &request.target.address.action,
                request.argument.to_slot(),
                confirmation,
            )
            .map_err(bridge_error)?;
        let reply = tokio::time::timeout(std::time::Duration::from_secs(5), reply)
            .await
            .map_err(|_| ActionHostError::Unavailable {
                reason: "admission timed out and may be unknown; observe before retrying".into(),
            })?
            .map_err(|_| ActionHostError::Unavailable {
                reason: "the action directory is unavailable".into(),
            })?
            .map_err(bridge_error)?;
        match reply {
            ActionDirectoryBridgeResponse::StartedDirectoryAction(started) => Ok(started),
            _ => Err(ActionHostError::Unavailable {
                reason: "the action directory answered with something else".into(),
            }),
        }
    }
}
