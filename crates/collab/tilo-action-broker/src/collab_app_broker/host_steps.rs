//! Steps the host itself runs for the person at this device: pairing a
//! nearby device, administering the person's identities and the devices
//! trusted under them, and changing the person's settings.
//!
//! Each is answered under the host's own provider, offered only to a caller
//! on this machine confined to a workspace, and admitted against that workspace's
//! policy under the service's app id — the application whose controls reach
//! the steps as the person using it. The door treats them alike; only the
//! service that runs a step differs.

use std::sync::Arc;

use super::AppBrokerState;
use crate::services::{HostServices, HostStepOutcome, HostStepRequest, HostStepServices};
use tilo_actions::remote::{
    ActionHostError, Caller, InvocationReceipt, InvokeRequest, Pending, Reservation,
};

impl AppBrokerState {
    /// Admit a host step marked for turns ([`HostStepServices::for_turns`]) as the turn's own,
    /// exactly as the same agent's app action in that turn is admitted: the run the agent works
    /// for the turn's message, decided by [`TrackerRuns::authorize_action`] over the person's,
    /// the agent's and the run's live grants, and refused in the same words the app action
    /// hears. A caller working no turn takes none of them — unless it works in a run of its own
    /// (`run`, an agent door connection's [`super::TaskRunHost`]): that run is what admits it,
    /// turn or none, exactly as it admits the connection's app actions.
    ///
    /// # Errors
    /// The caller works no turn and no run, the turn or run ended, or its grants refuse the step.
    fn admit_turn_step(
        &self,
        caller: &Caller,
        workspace: &str,
        steps: &dyn HostStepServices,
        (descriptor, argument): (&tilo_actions::ActionDescriptor, &Option<String>),
        (run, turn): (
            Option<&super::TaskRunHost>,
            Option<&tilo_action_policy::turn::Turn>,
        ),
    ) -> Result<(), ActionHostError> {
        let actor = super::BrokerActor::for_caller(caller);
        let turn = turn.or_else(|| actor.turn());
        if run.is_none() && turn.is_none() {
            return Err(ActionHostError::Refused {
                reason: "a session step is a step of a turn's: this agent works no turn".into(),
            });
        }
        let agent = actor.id().trim_start_matches("agent:");
        let service = self
            .tracker_runs(workspace)
            .ok_or_else(|| ActionHostError::Unavailable {
                reason: "this workspace's turns are not at hand".into(),
            })?;
        let run = service
            .runs(workspace)
            .map_err(|reason| ActionHostError::Unavailable { reason })?
            .into_iter()
            .find(|held| {
                held.active
                    && held.agent == agent
                    && match run {
                        // The run the connection works in, by its issue: its turn, if it has
                        // one, is the run's own.
                        Some(host) => held.issue == host.issue(),
                        None => held.turn.as_ref() == turn,
                    }
            })
            .ok_or_else(|| ActionHostError::Refused {
                reason: format!("the turn this step answers is no longer going ({agent})"),
            })?;
        service
            .check_agent(workspace, &run)
            .map_err(|reason| ActionHostError::Refused { reason })?;
        let requirement = crate::services::NativeTaskActionRequirement {
            binding: crate::services::NativeTaskBinding {
                task_id: run.issue.clone(),
                agent_id: run.agent.clone(),
                epoch: run.epoch,
                revision: run.applied_revision(),
                starter: Some(run.starter.clone()),
                turn: run.turn.clone(),
                origin: run.origin.clone(),
            },
            app: steps.app_id().to_owned(),
            resource_kind: descriptor.resource_kind.clone(),
            resource: descriptor.address.resource.clone(),
            action: descriptor.address.action.clone(),
            need: descriptor.contract.need(),
            consequential: descriptor.contract.asks_for_argument(argument.as_deref()),
            attended: descriptor.contract.is_attended(),
            confirmed: false,
        };
        service
            .authorize_action(workspace, &run, &requirement)
            .map_err(|reason| {
                if crate::services::NativeTaskActionRequirement::awaits_confirmation(&reason) {
                    ActionHostError::confirmation_required()
                } else {
                    ActionHostError::Refused { reason }
                }
            })
    }
}

/// The installed host-step service that answers under `provider`, if any.
pub(super) fn for_provider(
    services: &HostServices,
    provider: &str,
) -> Option<Arc<dyn HostStepServices>> {
    services
        .host_steps
        .iter()
        .find(|steps| steps.provider() == provider)
        .cloned()
}

/// Transport keys alone never grant access to a service on this device.
/// The certified local door retains its original admission, even when the
/// protected native seat projects a keyless person. Network admissions,
/// including same-person devices, are not that door.
pub(super) fn is_local_host_caller(
    caller: &Caller,
    origin: Option<&super::published_invocation::InvocationCaller>,
) -> bool {
    if caller.is_foreign_device() {
        return false;
    }
    if caller.device_public_key().is_none()
        && !matches!(
            origin,
            Some(super::published_invocation::InvocationCaller::Certified { .. })
        )
    {
        return match origin {
            Some(super::published_invocation::InvocationCaller::Local { admitted, .. }) => admitted
                .certificate
                .as_ref()
                .is_none_or(|cert| !cert.capabilities.foreign),
            Some(super::published_invocation::InvocationCaller::NativePerson {
                certificate,
                ..
            }) => !certificate.capabilities.foreign,
            Some(super::published_invocation::InvocationCaller::Certified { .. }) => false,
            None => true,
        };
    }
    let Some(super::published_invocation::InvocationCaller::Certified {
        caller: original,
        foreign_restriction: false,
    }) = origin
    else {
        return false;
    };
    !original.presentation.is_empty()
        && caller.workspace() == Some(original.workspace.as_str())
        && original
            .admitted
            .certificate
            .as_ref()
            .is_some_and(|certificate| {
                !certificate.capabilities.foreign
                    && if caller.device_public_key().is_none() {
                        original.native_projection_verified
                            && original.delegated.is_none()
                            && caller.user_public_key().is_none()
                            && caller.principal()
                                == &tilo_actions::remote::CallerPrincipal::Person(
                                    super::principal_id_for_root(certificate.user_public_key),
                                )
                    } else {
                        caller.user_public_key() == Some(&certificate.user_public_key)
                            && caller.device_public_key() == Some(&certificate.device_public_key)
                    }
            })
}

impl AppBrokerState {
    /// Admit one host step (pairing a nearby device, administering the
    /// person's identity) for `caller` and start it on the host.
    ///
    /// Host steps run on this machine, for the person at it: a remote device
    /// is never offered them, and the caller must be confined to the workspace its
    /// connection was admitted to, whose policy decides the step under the
    /// service's app id. A step whose contract asks first needs the
    /// person's own confirmation, or the answer they gave on their device.
    /// The host then decides whether the actor may take it at all. The
    /// invocation answers `Accepted` at once and records the outcome when
    /// the host's step ends, which for a pairing approval may be minutes
    /// later.
    pub(super) fn invoke_host_step(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&super::published_invocation::InvocationCaller>,
        steps: &dyn HostStepServices,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        self.invoke_host_step_in_run(caller, origin, steps, request, None)
    }

    /// As [`Self::invoke_host_step`], taken in a task's run `run` when it is
    /// one of a run's steps; the step is handed the run
    /// ([`HostStepRequest::task_run`]).
    pub(super) fn invoke_host_step_in_run(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&super::published_invocation::InvocationCaller>,
        steps: &dyn HostStepServices,
        request: &InvokeRequest,
        run: Option<&super::TaskRunHost>,
    ) -> Result<InvocationReceipt, ActionHostError> {
        self.invoke_host_step_within(caller, origin, steps, request, (run, None))
    }

    /// [`Self::invoke_host_step_in_run`], with the turn a step marked for turns answers
    /// (`turn`: the caller's, which a run host's connection does not carry on its caller).
    pub(super) fn invoke_host_step_in_turn(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&super::published_invocation::InvocationCaller>,
        steps: &dyn HostStepServices,
        request: &InvokeRequest,
        run: Option<&super::TaskRunHost>,
        turn: Option<&tilo_action_policy::turn::Turn>,
    ) -> Result<InvocationReceipt, ActionHostError> {
        self.invoke_host_step_within(caller, origin, steps, request, (run, turn))
    }

    /// [`Self::invoke_host_step_in_run`]'s body, with the caller's turn beside the run.
    #[expect(
        clippy::too_many_lines,
        reason = "one ordered admission of a host step: workspace confinement, the turn's policy, confirmation and the host's own decision"
    )]
    fn invoke_host_step_within(
        self: &Arc<Self>,
        caller: &Caller,
        origin: Option<&super::published_invocation::InvocationCaller>,
        steps: &dyn HostStepServices,
        request: &InvokeRequest,
        (run, turn): (
            Option<&super::TaskRunHost>,
            Option<&tilo_action_policy::turn::Turn>,
        ),
    ) -> Result<InvocationReceipt, ActionHostError> {
        let abandon = |error: ActionHostError| {
            if let Ok(mut ledger) = self.remote_invocations.lock() {
                ledger.abandon(&request.invocation);
            }
            error
        };
        let reservation = self.reserve_invocation(caller, request)?;
        if matches!(reservation, Reservation::Admitted) {
            return self
                .remote_job(caller, &request.invocation)
                .map(|job| self.receipt_for(request.invocation, job))
                .ok_or(ActionHostError::UnknownInvocation);
        }
        if !is_local_host_caller(caller, origin) {
            return Err(abandon(ActionHostError::UnknownAction));
        }
        let Some(workspace_id) = caller.workspace() else {
            return Err(abandon(ActionHostError::NotPermitted));
        };
        let address = &request.target.address;
        let argument = request.argument.to_slot();
        let Some(descriptor) = steps.invocation(address, argument.as_deref()) else {
            return Err(abandon(ActionHostError::UnknownAction));
        };
        let (workspace, policy, now) = self
            .current_framework_authority(caller, origin, workspace_id)
            .map_err(abandon)?;
        // One answer covers both gates of this exact host invocation.
        let witnessed = self.consume_answer(
            request.confirmation.as_ref(),
            &self.witness_binding(policy.id(), workspace_id, request),
        );
        // A step marked for turns is a turn's step for an agent, decided by the turn's run
        // exactly as the same agent's app action in that turn is: the tracker's
        // `authorize_action` over the person's, the agent's and the run's live grants. An agent
        // outside a turn takes none of them, and one whose grant refuses it hears the refusal an
        // app action hears. The person at this seat needs no turn — their own input is the
        // authority a turn borrows — so their step is decided as every other step is, by the
        // workspace's grants and the native seat's custody. For an agent the turn's grants are
        // the whole answer, as they are for the app action: `framework_policy` decides by the
        // workspace's grants alone, and asking it again would hold a step to the person's own
        // record where the turn already held it to the same person's live grants.
        let by_turn = steps.for_turns() && policy.is_agent();
        let turn = turn.or_else(|| caller.turn());
        if by_turn {
            self.admit_turn_step(
                caller,
                workspace_id,
                steps,
                (&descriptor, &argument),
                (run, turn),
            )
            .map_err(abandon)?;
        } else {
            self.framework_policy(
                (&workspace, &policy, now),
                (steps.app_id(), &descriptor),
                request.confirmation.as_ref(),
                witnessed,
                false,
            )
            .map_err(abandon)?;
        }
        if !Self::confirmed_as_asked(
            &policy,
            &descriptor,
            argument.as_deref(),
            request.confirmation.as_ref(),
            witnessed,
        ) {
            return Err(abandon(ActionHostError::confirmation_required()));
        }
        let admitted = self
            .admit_host_action(
                caller,
                origin,
                super::host_action_admission::HostActionAdmission {
                    workspace: workspace_id,
                    app_id: steps.app_id(),
                    descriptor: &descriptor,
                    request,
                },
            )
            .map_err(abandon)?;
        let id = admitted.id;
        if let Ok(mut ledger) = self.remote_invocations.lock() {
            ledger.complete(&request.invocation, id);
        }
        // Signing and journalling may block. Resolve original custody and the
        // committed policy again for the effect, not the pre-admission actor.
        let policy = self.framework_effect_ready(&admitted, || {
            let (workspace, policy, now) =
                self.current_framework_authority(caller, origin, workspace_id)?;
            if !Self::confirmed_as_asked(
                &policy,
                &descriptor,
                argument.as_deref(),
                request.confirmation.as_ref(),
                witnessed,
            ) {
                return Err(ActionHostError::confirmation_required());
            }
            if by_turn {
                // The turn still goes, and still lets this agent take this step.
                self.admit_turn_step(
                    caller,
                    workspace_id,
                    steps,
                    (&descriptor, &argument),
                    (run, turn),
                )?;
            } else {
                self.admit_answer(
                    (&workspace, &policy, now),
                    steps.app_id(),
                    &descriptor,
                    (argument.as_deref(), request.confirmation.as_ref()),
                    witnessed,
                )?;
            }
            Ok(policy)
        })?;
        let step = HostStepRequest {
            workspace: workspace_id,
            actor: &policy,
            address,
            argument: argument.as_deref(),
            task_run: run.map(|run| run as &dyn tilo_broker_core::steps::TaskRun),
            run: request
                .context
                .as_ref()
                .and_then(|context| context.group.as_ref()),
        };
        if descriptor.contract.is_live() {
            let view = match steps.watch(&step) {
                Ok(view) => view,
                Err(reason) => {
                    self.complete_host_action(
                        &admitted,
                        host_step_failure(request.invocation, "host_step_refused", &reason),
                    )?;
                    return Err(ActionHostError::Refused { reason });
                }
            };
            let following = super::live_views::Following {
                caller: caller.clone(),
                origin: origin.cloned(),
                workspace: workspace_id.to_owned(),
                app_id: steps.app_id().to_owned(),
                descriptor,
                witnessed,
            };
            self.follow_live_view(admitted, following, view)
                .map_err(|reason| ActionHostError::Refused { reason })?;
            return Ok(self.receipt_for(request.invocation, id));
        }
        let started = steps.start(&step);
        match started {
            Ok(started) => self.follow_admitted_host_step(admitted, started),
            Err(reason) => {
                self.complete_host_action(
                    &admitted,
                    host_step_failure(request.invocation, "host_step_refused", &reason),
                )?;
                return Err(ActionHostError::Refused { reason });
            }
        }
        Ok(self.receipt_for(request.invocation, id))
    }

    /// Follow an already durable admission without replacing its caller or UUID.
    pub(super) fn follow_admitted_host_step(
        self: &Arc<Self>,
        admitted: super::host_action_admission::AdmittedHostAction,
        started: Pending<HostStepOutcome>,
    ) {
        let state = Arc::clone(self);
        started.then(move |outcome| {
            let invocation = admitted.invocation;
            let uncertain = outcome.is_none();
            let update = match outcome {
                Some(Ok(output)) => {
                    tilo_actions::InvocationUpdate::Succeeded { invocation, output }
                }
                Some(Err(error)) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: *error,
                },
                None => host_step_failure(
                    invocation,
                    "host_step_stopped",
                    "the host step stopped without an answer",
                ),
            };
            let completed = if uncertain {
                state.complete_uncertain_host_action(&admitted, update)
            } else {
                state.complete_host_action(&admitted, update)
            };
            if let Err(error) = completed {
                tracing::warn!(?error, "could not persist host step completion");
            }
        });
    }
}

fn host_step_failure(
    invocation: tilo_actions::InvocationId,
    code: &str,
    message: &str,
) -> tilo_actions::InvocationUpdate {
    tilo_actions::InvocationUpdate::Failed {
        invocation,
        error: tilo_actions::ActionFailure {
            code: code.into(),
            message: message.into(),
            details: None,
            retryable: true,
        },
    }
}

#[cfg(test)]
pub(crate) mod signed_test_support {
    use super::super::{
        AppBrokerState, BrokerChannels, DurableInvocation, FanoutState, InvocationKey,
        PeerPublisher, principal_id_for_root, published_invocation::InvocationCaller,
    };
    use crate::services::HostServices;
    use crate::test_support::{Workspace, WorkspaceSlot, WorkspaceTree};
    use ed25519_dalek::Signer as _;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use tilo_actions::remote::{ActionHostError, Caller, InvocationReceipt, InvokeRequest};

    struct NativeOwnerReceipts {
        owner: Arc<tilo_peer_state::owner::NativeWorkspaceOwner>,
    }

    impl tilo_peer_state::owner_commit::OwnerCommitService for NativeOwnerReceipts {
        fn health(&self) -> Result<(), String> {
            let workspace =
                tilo_desktop_model::WorkspaceId::parse(tilo_peer_state::owner::testing::WORKSPACE)
                    .unwrap();
            self.owner.with_current(workspace, |_| Ok(()))
        }

        fn commit(
            &self,
            request: tilo_peer_state::owner_commit::OwnerCommitRequest,
        ) -> tilo_peer_state::owner_commit::CommitFuture {
            let owner = self.owner.clone();
            Box::pin(async move { owner.commit_effect_receipt(&request, tilo_clock::wall()) })
        }
    }

    pub(crate) struct Authority {
        certificate: tilo_user_identity::DeviceCert,
        pub revoked: AtomicBool,
        pub device_governs: AtomicBool,
        time: Mutex<crate::policy_clock::PolicyTime>,
        replace_time_on_authorize: Mutex<Option<crate::policy_clock::PolicyTime>>,
    }

    impl Authority {
        pub fn replace_time_on_authorize(&self, time: crate::policy_clock::PolicyTime) {
            *self.replace_time_on_authorize.lock().unwrap() = Some(time);
        }
    }

    impl crate::certificate_authority::CertificateAuthority for Authority {
        fn governs(&self, _: uuid::Uuid, device: Option<&[u8; 32]>) -> bool {
            device.is_none() || self.device_governs.load(Ordering::SeqCst)
        }

        fn policy_time(&self) -> Result<Option<crate::policy_clock::PolicyTime>, String> {
            Ok(Some(self.time.lock().unwrap().clone()))
        }

        fn authorize(
            &self,
            _workspace: uuid::Uuid,
            certificate: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            let replacement = self.replace_time_on_authorize.lock().unwrap().take();
            if let Some(time) = replacement {
                *self.time.lock().unwrap() = time;
            }
            if self.revoked.load(Ordering::SeqCst) || certificate != &self.certificate {
                return Err("device is no longer enrolled".into());
            }
            tilo_user_identity::verify_device_cert(certificate).map_err(|error| error.to_string())
        }
    }

    pub(crate) struct Fixture {
        pub state: Arc<AppBrokerState>,
        pub caller: Caller,
        pub origin: InvocationCaller,
        pub authority: Arc<Authority>,
        pub workspace: String,
        pub tree: crate::test_support::WorkspaceTreeHandle,
        certified_host: Option<Arc<dyn tilo_actions::remote::ActionHost>>,
        _fanout: Arc<FanoutState>,
        _environment: tilo_test_env::ScopedEnv,
        directory: tempfile::TempDir,
        clock: crate::policy_clock::OwnerPolicyClock,
        policy_origin: crate::policy_clock::PolicyOrigin,
    }

    impl Fixture {
        pub fn new_policy_origin(&self) -> crate::policy_clock::PolicyOrigin {
            self.clock.new_origin()
        }

        pub fn retire_policy_origin(&self) {
            self.policy_origin.invalidate();
        }

        pub fn new(services: HostServices) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let mut environment = tilo_test_env::ScopedEnv::new();
            environment
                .set("XDG_CONFIG_HOME", directory.path().join("config"))
                .set("TILO_IDENTITY_ROOT", directory.path().join("identity"))
                .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
            tilo_identity_host::provisioning::ensure_collaboration_identity(
                &directory.path().join("config/tilo/identity"),
                &tilo_user_identity::IdentityPaths::under(directory.path().join("identity")),
                tilo_identity_host::provisioning::IdentityStorage::Files,
            )
            .expect("provision the production receipt signer");
            let mut workspace = Workspace::in_workspace(
                WorkspaceSlot(9),
                tilo_desktop_model::WorkspaceId::parse(tilo_peer_state::owner::testing::WORKSPACE)
                    .unwrap(),
                "Host actions",
            );
            let workspace_id = workspace.id().to_string();
            let (key, certificate) =
                tilo_identity_host::broker::trusted_current_device(&workspace_id).unwrap();
            let owner = Arc::new(
                tilo_peer_state::owner::open_file(&directory.path().join("native.json")).unwrap(),
            );
            tilo_peer_state::owner::testing::enroll_interactive_governor(
                &owner,
                &key,
                &certificate,
            )
            .unwrap();
            let publisher = PeerPublisher::with_optional_owner_commit(
                None,
                Arc::new(NativeOwnerReceipts { owner }),
            );
            let person = principal_id_for_root(certificate.user_public_key);
            workspace.set_owner(person.clone());
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(workspace);
            let tree = tree.shared();
            let clock = crate::policy_clock::OwnerPolicyClock::new().unwrap();
            let policy_origin = clock.new_origin();
            let authority = Arc::new(Authority {
                certificate: certificate.clone(),
                revoked: AtomicBool::new(false),
                device_governs: AtomicBool::new(true),
                time: Mutex::new(policy_origin.capture().unwrap()),
                replace_time_on_authorize: Mutex::new(None),
            });
            let config = crate::NativePeerBrokerConfig {
                journal_root: directory.path().join("journal"),
                certificate_authority: authority.clone(),
            };
            let fanout = Arc::new(FanoutState::new());
            let (state, _) = AppBrokerState::new_configured(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                publisher,
                Some(tree.authority()),
                None,
                Some(&config),
                services,
            );
            let presentation = runtime_collab::CertifiedJoinIdentity {
                workspace_id: workspace_id.clone(),
                device_certificate: serde_json::to_vec(&certificate).unwrap(),
                proof: key
                    .sign(&runtime_identity::collaboration_join_signing_bytes(
                        &workspace_id,
                    ))
                    .to_bytes()
                    .to_vec(),
            }
            .encode()
            .unwrap();
            let verified = state
                .verify_action_caller(&workspace_id, &presentation)
                .unwrap();
            let origin = InvocationCaller::Local {
                route: super::super::CrdtRoute::cluster(&workspace_id),
                admitted: Box::new(verified.admitted),
                time: state.registered_policy_time().unwrap(),
            };
            Self {
                state,
                caller: Caller::local_person(person).in_workspace(workspace_id.clone()),
                origin,
                authority,
                workspace: workspace_id,
                tree,
                certified_host: None,
                _fanout: fanout,
                _environment: environment,
                directory,
                clock,
                policy_origin,
            }
        }

        pub fn new_certified(services: HostServices) -> Self {
            Self::certified(services, false)
        }

        pub fn new_certified_foreign(services: HostServices) -> Self {
            Self::certified(services, true)
        }

        fn certified(services: HostServices, foreign: bool) -> Self {
            let mut fixture = Self::new(services);
            let (key, certificate) =
                tilo_identity_host::broker::trusted_current_device(&fixture.workspace).unwrap();
            let presentation = runtime_collab::CertifiedJoinIdentity {
                workspace_id: fixture.workspace.clone(),
                device_certificate: serde_json::to_vec(&certificate).unwrap(),
                proof: key
                    .sign(&runtime_identity::collaboration_join_signing_bytes(
                        &fixture.workspace,
                    ))
                    .to_bytes()
                    .to_vec(),
            }
            .encode()
            .unwrap();
            fixture.caller =
                Caller::authenticated(certificate.user_public_key, certificate.device_public_key)
                    .in_workspace(&fixture.workspace)
                    .from_foreign_device(foreign)
                    .in_new_execution();
            fixture.certified_host = Some(
                fixture
                    .state
                    .certified_local_action_host(&fixture.caller, &presentation)
                    .unwrap(),
            );
            fixture.origin = InvocationCaller::Certified {
                caller: Box::new(
                    fixture
                        .state
                        .verify_action_caller(&fixture.workspace, &presentation)
                        .unwrap(),
                ),
                foreign_restriction: foreign,
            };
            fixture
        }

        fn owner(&self) -> super::super::InvocationOwner {
            match &self.origin {
                InvocationCaller::Certified { caller, .. } => caller.owner().unwrap(),
                InvocationCaller::Local { admitted, .. } => {
                    let certificate = admitted.certificate.as_ref().unwrap();
                    super::super::InvocationOwner::AuthenticatedLocal {
                        root: certificate.user_public_key,
                        device: certificate.device_public_key,
                        agent: None,
                    }
                }
                InvocationCaller::NativePerson { .. } => panic!("authenticated custody"),
            }
        }

        pub fn invoke(
            &self,
            request: &InvokeRequest,
        ) -> Result<InvocationReceipt, ActionHostError> {
            let pending = match &self.certified_host {
                Some(host) => host.invoke(&self.caller, request.clone()),
                None => self.state.action_invoke_with_origin(
                    &self.caller,
                    None,
                    None,
                    Some(&self.origin),
                    request,
                ),
            };
            pending
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .expect("host admission answers immediately")
        }

        pub fn query(&self, provider: &str) -> tilo_actions::remote::CataloguePage {
            let query = tilo_actions::remote::CatalogueQuery {
                provider: Some(provider.into()),
                ..Default::default()
            };
            let pending = match &self.certified_host {
                Some(host) => host.query(&self.caller, query),
                None => {
                    self.state
                        .action_query_with_origin(&self.caller, Some(&self.origin), &query)
                }
            };
            pending
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .expect("catalogue answers immediately")
                .expect("the certified caller may query")
        }

        #[allow(
            clippy::significant_drop_tightening,
            reason = "the test keeps its fixture and guards alive through its assertions"
        )]
        pub fn admission_check(&self, request: &InvokeRequest) -> Box<dyn Fn() + Send> {
            let key = InvocationKey {
                workspace: self.workspace.clone(),
                host: self.state.invocation_host().unwrap(),
                owner: self.owner(),
                invocation: request.invocation,
            };
            let state = Arc::downgrade(&self.state);
            Box::new(move || {
                let state = state.upgrade().expect("host is live at dispatch");
                let guard = state.journal.lock().unwrap();
                let journal = guard.as_ref().unwrap().as_ref().unwrap();
                let record = journal
                    .lookup(&key)
                    .unwrap()
                    .expect("durable before dispatch");
                assert!(record.admission.is_some(), "signed before dispatch");
                assert!(record.terminal.is_none(), "not yet completed");
                assert!(
                    matches!(record.update, tilo_actions::InvocationUpdate::Accepted { invocation } if invocation == key.invocation)
                );
            })
        }

        #[allow(
            clippy::significant_drop_tightening,
            reason = "the test keeps its fixture and guards alive through its assertions"
        )]
        pub fn record(&self, request: &InvokeRequest) -> DurableInvocation {
            let guard = self.state.journal.lock().unwrap();
            let journal = guard.as_ref().unwrap().as_ref().unwrap();
            journal
                .lookup(&InvocationKey {
                    workspace: self.workspace.clone(),
                    host: journal.host(),
                    owner: self.owner(),
                    invocation: request.invocation,
                })
                .unwrap()
                .expect("durable host admission")
        }

        pub fn fail_next_barrier(&self) {
            self.state
                .journal
                .lock()
                .unwrap()
                .as_mut()
                .unwrap()
                .as_mut()
                .unwrap()
                .fail_at = Some(super::super::invocation_journal::BarrierFailure::File);
        }

        pub fn reopen_journal(&self) {
            let mut guard = self.state.journal.lock().unwrap();
            drop(guard.as_mut().unwrap().take());
            *guard = Ok(Some(
                super::super::InvocationJournal::open(&self.directory.path().join("journal"))
                    .unwrap(),
            ));
        }

        pub fn assert_signed(&self, request: &InvokeRequest, terminal: bool) -> DurableInvocation {
            let record = self.record(request);
            let admission = record.admission.as_ref().expect("signed admission");
            let receipt = admission.public_receipt();
            crate::action_evidence::verify(receipt, &receipt.facts).unwrap();
            assert_eq!(receipt.facts.invocation, request.invocation);
            assert_eq!(receipt.facts.workspace, self.workspace);
            assert_eq!(receipt.facts.resource, request.target.address.resource);
            assert_eq!(receipt.facts.action, request.target.address.action);
            assert!(record.admitted_request_is(&serde_json::to_value(request).unwrap()));
            if terminal {
                crate::action_evidence::verify_terminal(
                    record.terminal.as_ref().expect("signed terminal"),
                    receipt,
                )
                .unwrap();
            } else {
                assert!(record.terminal.is_none(), "admission is not execution");
            }
            record
        }
    }
}

#[cfg(test)]
mod identity_admin_tests {
    use super::super::AppBrokerState;
    use super::super::action_host::RowScope;
    use crate::services::{HostStepOutcome, HostStepRequest, HostStepServices};
    use crate::test_support::{Grants as _, Workspace, WorkspaceSlot, WorkspaceTree};
    use std::sync::{Arc, Mutex};
    use tilo_actions::remote::{
        ActionHostError, Caller, Completer, Confirmation, InvokeArgument, InvokeRequest, Pending,
        RemoteActionMessage, identity_admin, pending,
    };

    /// The host's identity store, standing in: it records each step it was
    /// asked to start and answers when the test completes it.
    #[derive(Default)]
    struct Host {
        started: Mutex<Vec<(String, String, String)>>,
        answers: Mutex<Vec<Completer<HostStepOutcome>>>,
        admission_check: Mutex<Option<Box<dyn Fn() + Send>>>,
    }

    fn contract(step: &str) -> tilo_actions::ActionContract {
        match step {
            identity_admin::LIST | identity_admin::DEVICES => tilo_actions::ActionContract::view(),
            identity_admin::HOME_PEER_RETRY => tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Outside,
                ..tilo_actions::ActionContract::default()
            },
            _ => tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Outside,
                risk: tilo_actions::RiskLevel::Privileged,
                confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                ..tilo_actions::ActionContract::default()
            },
        }
    }

    impl HostStepServices for Host {
        fn provider(&self) -> &'static str {
            identity_admin::PROVIDER
        }

        fn app_id(&self) -> &'static str {
            "so.tilo.settings"
        }

        fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
            identity_admin::STEPS
                .into_iter()
                .map(|step| tilo_actions::ActionDescriptor {
                    state: tilo_actions::ActionState::default(),
                    semantic_identity: None,
                    address: tilo_actions::ActionAddress::new(identity_admin::RESOURCE, step),
                    resource_kind: identity_admin::RESOURCE.into(),
                    resource_label: "Identities".into(),
                    action_label: step.into(),
                    keywords: Vec::new(),
                    icon: None,
                    description: None,
                    locus: tilo_actions::ExecutionLocus::default(),
                    kind: tilo_actions::OperationKind::Effect,
                    contract: contract(step),
                    input: tilo_actions::InputSpec::None,
                    params: Vec::new(),
                    view: None,
                })
                .collect()
        }

        fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
            if let Some(check) = self.admission_check.lock().unwrap().as_ref() {
                check();
            }
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

    fn broker(host: Arc<Host>) -> (Arc<AppBrokerState>, String, String) {
        let owner = hex::encode([41; 32]);
        let mut workspace = Workspace::new(WorkspaceSlot(9), "Identities");
        workspace.set_owner(owner.clone());
        let workspace_id = workspace.id().to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge_rx) = AppBrokerState::test_state(
            Some(tree.shared().authority()),
            crate::services::HostServices {
                host_steps: vec![host],
                ..Default::default()
            },
        );
        (state, owner, workspace_id)
    }

    /// A native host's seat, which never certifies anyone here.
    struct NoSeat;

    impl crate::services::LocalPersonServices for NoSeat {
        fn certificate(
            &self,
            _workspace: &str,
            _person: &str,
        ) -> Result<tilo_user_identity::DeviceCert, String> {
            Err("no seat in this test".into())
        }
    }

    /// On a native host (one with the person's seat), neither an agent nor a
    /// person that carries no retained custody takes a host step: an agent
    /// takes one only in its task run (`task_run_host`), a person only
    /// through the seat. Neither is taken as an actor-only caller.
    #[test]
    fn a_keyless_agent_or_person_takes_no_host_step_on_a_native_host() {
        let host = Arc::new(Host::default());
        let owner = hex::encode([41; 32]);
        let mut workspace = Workspace::new(WorkspaceSlot(9), "Identities");
        workspace.set_owner(owner.clone());
        workspace.allow_agent("planner", crate::test_support::KindSet::READ);
        let workspace_id = workspace.id().to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge_rx) = AppBrokerState::test_state(
            Some(tree.shared().authority()),
            crate::services::HostServices {
                host_steps: vec![host.clone()],
                local_person: Some(Arc::new(NoSeat)),
                ..Default::default()
            },
        );
        let agent = Caller::local_agent("planner").in_workspace(workspace_id.clone());
        assert!(
            invoked(&state, &agent, &request(identity_admin::LIST, None)).is_err(),
            "an agent outside any run takes no host step"
        );
        let person = Caller::local_person(owner).in_workspace(workspace_id);
        assert!(
            invoked(&state, &person, &request(identity_admin::LIST, None)).is_err(),
            "a person reaches a native host's steps only with the seat's custody"
        );
        assert!(host.started.lock().unwrap().is_empty());
    }

    fn request(step: &str, confirmation: Option<Confirmation>) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: identity_admin::target(step),
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
    ) -> Result<tilo_actions::remote::InvocationReceipt, ActionHostError> {
        state
            .action_invoke(caller, None, None, request)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("admission answers at once")
    }

    #[derive(Default)]
    struct PairingHost(Host);

    impl HostStepServices for PairingHost {
        fn provider(&self) -> &'static str {
            tilo_actions::remote::device_pairing::PROVIDER
        }

        fn app_id(&self) -> &str {
            self.0.app_id()
        }

        fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
            let mut descriptor = self
                .0
                .descriptors()
                .into_iter()
                .find(|descriptor| descriptor.address.action == identity_admin::REMOVE)
                .unwrap();
            descriptor.address = tilo_actions::remote::device_pairing::target(
                tilo_actions::remote::device_pairing::APPROVE,
            )
            .address;
            descriptor.resource_kind = tilo_actions::remote::device_pairing::RESOURCE.into();
            vec![descriptor]
        }

        fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
            self.0.start(request)
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the test keeps its fixture and guards alive through its assertions"
    )]
    /// Asking only who publishes (`max_results: 0`, how an agent's paths
    /// find an app) lists the host's own services beside apps' windows, and
    /// builds no row; asking a service for its rows still gets them.
    #[test]
    fn a_host_service_is_listed_when_only_publishers_are_asked() {
        let host = Arc::new(Host::default());
        let (state, owner, workspace) = broker(host);
        let caller = Caller::local_person(owner).in_workspace(workspace.clone());
        let ask = |query: tilo_actions::remote::CatalogueQuery| {
            state
                .action_query(&caller, &query)
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .expect("the catalogue answers at once")
                .expect("the workspace's owner may ask")
        };
        let publishers = ask(tilo_actions::remote::CatalogueQuery {
            max_results: 0,
            ..Default::default()
        });
        assert!(publishers.rows.is_empty(), "{:?}", publishers.rows);
        assert!(
            publishers.providers.iter().any(|provider| {
                provider.provider == identity_admin::PROVIDER
                    && provider.app_id == "so.tilo.settings"
                    && provider.workspace == workspace
                    && provider.browsable
            }),
            "{:?}",
            publishers.providers
        );
        let rows = ask(tilo_actions::remote::CatalogueQuery {
            provider: Some(identity_admin::PROVIDER.into()),
            ..Default::default()
        });
        assert!(
            rows.rows
                .iter()
                .any(|row| row.provider == identity_admin::PROVIDER),
            "{:?}",
            rows.rows
        );
    }

    #[test]
    fn certified_host_steps_admit_only_nonforeign_local_custody() {
        for pairing in [false, true] {
            for foreign in [false, true] {
                let host = Arc::new(PairingHost::default());
                let identity = Arc::new(Host::default());
                let services = crate::services::HostServices {
                    host_steps: vec![if pairing {
                        host.clone() as Arc<dyn HostStepServices>
                    } else {
                        identity.clone()
                    }],
                    ..Default::default()
                };
                let f = if foreign {
                    super::signed_test_support::Fixture::new_certified_foreign(services)
                } else {
                    super::signed_test_support::Fixture::new_certified(services)
                };
                let mut asked = request(identity_admin::REMOVE, Some(Confirmation::Confirmed));
                if pairing {
                    asked.target = tilo_actions::remote::device_pairing::target(
                        tilo_actions::remote::device_pairing::APPROVE,
                    );
                }
                let service = if pairing { &host.0 } else { identity.as_ref() };
                assert_eq!(
                    f.query(&asked.target.provider).rows.is_empty(),
                    foreign,
                    "only the retained local door offers host steps"
                );
                assert!(
                    f.state
                        .eager_catalogue(
                            &f.caller,
                            super::super::action_host::RowScope::Provider(&asked.target.provider),
                        )
                        .0
                        .is_empty(),
                    "transport keys alone do not offer local host steps"
                );
                if foreign {
                    assert!(f.invoke(&asked).is_err());
                    assert!(service.started.lock().unwrap().is_empty());
                    continue;
                }
                assert!(
                    invoked(&f.state, &f.caller, &asked).is_err(),
                    "device keys without certified local custody are not a local door"
                );
                assert!(service.started.lock().unwrap().is_empty());
                *service.admission_check.lock().unwrap() = Some(f.admission_check(&asked));
                f.invoke(&asked).unwrap();
                f.assert_signed(&asked, false);
                assert_eq!(service.started.lock().unwrap().len(), 1);
                service
                    .answers
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap()
                    .complete(Ok(tilo_actions::ActionOutput::default()));
                f.assert_signed(&asked, true);
                f.invoke(&asked).unwrap();
                assert_eq!(service.started.lock().unwrap().len(), 1);
                f.authority
                    .revoked
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                asked.invocation = tilo_actions::InvocationId::new();
                assert!(f.invoke(&asked).is_err());
                drop(f);
                assert_eq!(service.started.lock().unwrap().len(), 1);
            }
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the test keeps its fixture and guards alive through its assertions"
    )]
    #[test]
    fn device_pairing_uses_the_same_signed_admission_and_terminal_path() {
        let host = Arc::new(PairingHost::default());
        let f = super::signed_test_support::Fixture::new(crate::services::HostServices {
            host_steps: vec![host.clone()],
            ..Default::default()
        });
        let mut asked = request(identity_admin::REMOVE, Some(Confirmation::Confirmed));
        asked.target = tilo_actions::remote::device_pairing::target(
            tilo_actions::remote::device_pairing::APPROVE,
        );
        *host.0.admission_check.lock().unwrap() = Some(f.admission_check(&asked));
        f.invoke(&asked).unwrap();
        f.assert_signed(&asked, false);
        assert_eq!(host.0.started.lock().unwrap().len(), 1);
        host.0
            .answers
            .lock()
            .unwrap()
            .pop()
            .unwrap()
            .complete(Ok(tilo_actions::ActionOutput::default()));
        let completed = f.assert_signed(&asked, true);
        assert!(
            matches!(completed.update, tilo_actions::InvocationUpdate::Succeeded { invocation, .. } if invocation == asked.invocation)
        );
        f.invoke(&asked).unwrap();
        assert_eq!(host.0.started.lock().unwrap().len(), 1);
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the test keeps its fixture and guards alive through its assertions"
    )]
    #[test]
    fn identity_admin_signs_before_start_and_retains_the_original_uuid() {
        let host = Arc::new(Host::default());
        let f = super::signed_test_support::Fixture::new(crate::services::HostServices {
            host_steps: vec![host.clone()],
            ..Default::default()
        });
        let asked = request(identity_admin::REMOVE, Some(Confirmation::Confirmed));
        *host.admission_check.lock().unwrap() = Some(f.admission_check(&asked));
        let receipt = f.invoke(&asked).unwrap();
        let accepted = f.assert_signed(&asked, false);
        assert_eq!(receipt.invocation, asked.invocation);
        assert!(
            matches!(accepted.update, tilo_actions::InvocationUpdate::Accepted { invocation } if invocation == asked.invocation)
        );
        let repeated = f.invoke(&asked).unwrap();
        assert_eq!(receipt, repeated);
        assert_eq!(host.started.lock().unwrap().len(), 1);
        let mut changed = asked.clone();
        changed.argument = InvokeArgument::Text("another identity".into());
        assert!(
            f.invoke(&changed).is_err(),
            "changed retry cannot start another step"
        );
        host.answers
            .lock()
            .unwrap()
            .pop()
            .unwrap()
            .complete(Ok(tilo_actions::ActionOutput::default()));
        let completed = f.assert_signed(&asked, true);
        assert!(
            matches!(completed.update, tilo_actions::InvocationUpdate::Succeeded { invocation, .. } if invocation == asked.invocation)
        );
        assert_eq!(completed.admission, accepted.admission);
        f.reopen_journal();
        assert_eq!(f.assert_signed(&asked, true), completed);
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the test keeps its fixture and guards alive through its assertions"
    )]
    #[test]
    fn identity_admin_persists_failed_and_dropped_host_outcomes() {
        for dropped in [false, true] {
            let host = Arc::new(Host::default());
            let f = super::signed_test_support::Fixture::new(crate::services::HostServices {
                host_steps: vec![host.clone()],
                ..Default::default()
            });
            let asked = request(identity_admin::REMOVE, Some(Confirmation::Confirmed));
            f.invoke(&asked).unwrap();
            // The service has accepted the effect; losing its answer does not
            // prove that the effect failed or make it safe to repeat.
            assert_eq!(host.started.lock().unwrap().len(), 1);
            let answer = host.answers.lock().unwrap().pop().unwrap();
            if dropped {
                drop(answer);
            } else {
                answer.complete(Err(Box::new(tilo_actions::ActionFailure {
                    code: "identity_busy".into(),
                    message: "the identity is in use".into(),
                    details: None,
                    retryable: true,
                })));
            }
            let completed = f.assert_signed(&asked, true);
            assert!(
                matches!(completed.update, tilo_actions::InvocationUpdate::Failed { invocation, .. } if invocation == asked.invocation)
            );
            assert_eq!(completed.uncertain, dropped);
            assert_eq!(
                completed.terminal.as_ref().unwrap().category,
                if dropped {
                    crate::action_evidence::TerminalCategory::Uncertain
                } else {
                    crate::action_evidence::TerminalCategory::Failed
                }
            );
            f.reopen_journal();
            assert_eq!(f.assert_signed(&asked, true), completed);
            f.invoke(&asked).unwrap();
            assert_eq!(host.started.lock().unwrap().len(), 1);
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the test keeps its fixture and guards alive through its assertions"
    )]
    #[test]
    fn identity_admin_admission_and_completion_barriers_are_fail_closed() {
        for admission_failure in [false, true] {
            let host = Arc::new(Host::default());
            let f = super::signed_test_support::Fixture::new(crate::services::HostServices {
                host_steps: vec![host.clone()],
                ..Default::default()
            });
            let asked = request(identity_admin::REMOVE, Some(Confirmation::Confirmed));
            if admission_failure {
                f.fail_next_barrier();
                assert!(f.invoke(&asked).is_err());
                assert!(host.started.lock().unwrap().is_empty());
                assert!(f.state.remote_job(&f.caller, &asked.invocation).is_none());
            } else {
                let receipt = f.invoke(&asked).unwrap();
                let job = f.state.remote_job(&f.caller, &receipt.invocation).unwrap();
                f.fail_next_barrier();
                host.answers
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap()
                    .complete(Ok(tilo_actions::ActionOutput::default()));
                assert!(
                    matches!(f.state.invocation_status(job), Some(
                    tilo_actions::InvocationUpdate::Accepted { invocation }
                ) if invocation == asked.invocation),
                    "failed durability must not publish success"
                );
            }
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the test keeps its fixture and guards alive through its assertions"
    )]
    #[test]
    fn host_steps_recheck_live_authority_after_blocked_durable_admission() {
        use std::sync::{atomic::Ordering, mpsc};
        use tilo_actions::InvocationUpdate;

        for certified in [false, true] {
            for change in [
                "unchanged",
                "certificate",
                "execution",
                "permission",
                "time",
            ] {
                let host = Arc::new(Host::default());
                let services = crate::services::HostServices {
                    host_steps: vec![host.clone()],
                    ..Default::default()
                };
                let mut f = if certified {
                    super::signed_test_support::Fixture::new_certified(services)
                } else {
                    super::signed_test_support::Fixture::new(services)
                };
                f.caller = f.caller.clone().in_new_execution();
                let asked = request(identity_admin::REMOVE, Some(Confirmation::Confirmed));
                let (entered, blocked) = mpsc::sync_channel(0);
                let (release, resume) = mpsc::sync_channel(0);
                f.state
                    .journal
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .after_admission = Some(Box::new(move || {
                    entered.send(()).unwrap();
                    resume.recv_timeout(tilo_test_env::hang_bound()).unwrap();
                }));
                let result = std::thread::scope(|scope| {
                    let state = f.state.clone();
                    let caller = f.caller.clone();
                    let origin = f.origin.clone();
                    let steps = host.clone();
                    let asked = &asked;
                    let worker = scope.spawn(move || {
                        state.invoke_host_step(&caller, Some(&origin), steps.as_ref(), asked)
                    });
                    blocked.recv_timeout(tilo_test_env::hang_bound()).unwrap();
                    // The journal writer holds a signed admission but has not
                    // returned a dispatch permit. Change authority in that gap.
                    match change {
                        "certificate" => f.authority.revoked.store(true, Ordering::SeqCst),
                        "execution" => f.caller.retire_execution(),
                        "time" => f.retire_policy_origin(),
                        "permission" => f.tree.write().workspaces[0].set_owner("someone-else"),
                        _ => {}
                    }
                    release.send(()).unwrap();
                    worker.join().unwrap()
                });
                if change == "unchanged" {
                    result.expect("unchanged custody must still start");
                    assert_eq!(host.started.lock().unwrap().len(), 1);
                    host.answers
                        .lock()
                        .unwrap()
                        .pop()
                        .unwrap()
                        .complete(Ok(tilo_actions::ActionOutput::default()));
                    let record = f.assert_signed(&asked, true);
                    assert!(matches!(record.update, InvocationUpdate::Succeeded { .. }));
                } else {
                    assert!(result.is_err(), "{certified}/{change}");
                    assert!(
                        host.started.lock().unwrap().is_empty(),
                        "{certified}/{change}"
                    );
                    let record = f.assert_signed(&asked, true);
                    assert!(!record.uncertain);
                    assert!(
                        matches!(record.update, InvocationUpdate::Failed { ref error, .. }
                        if error.code == "host_action_refused")
                    );
                }
            }
        }
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the test keeps its fixture and guards alive through its assertions"
    )]
    #[test]
    fn identity_admin_refuses_revoked_custody_and_missing_signer_before_start() {
        for revoked in [false, true] {
            let host = Arc::new(Host::default());
            let f = super::signed_test_support::Fixture::new(crate::services::HostServices {
                host_steps: vec![host.clone()],
                ..Default::default()
            });
            if revoked {
                f.authority
                    .revoked
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            } else {
                std::fs::remove_file(
                    tilo_identity_host::provisioning::collaboration_identity_paths().device_privkey,
                )
                .unwrap();
            }
            let asked = request(identity_admin::REMOVE, Some(Confirmation::Confirmed));
            assert!(f.invoke(&asked).is_err());
            assert!(host.started.lock().unwrap().is_empty());
            assert!(f.state.remote_job(&f.caller, &asked.invocation).is_none());
        }
    }

    #[test]
    fn identity_admin_is_offered_to_settings_as_its_person_and_removal_asks_first() {
        let host = Arc::new(Host::default());
        let (state, owner, workspace) = broker(Arc::clone(&host));
        let person = Caller::local_person(owner.clone()).in_workspace(workspace.clone());

        let rows = state
            .eager_catalogue(&person, RowScope::Provider(identity_admin::PROVIDER))
            .0;
        assert_eq!(
            rows.len(),
            identity_admin::STEPS.len(),
            "every step is offered to the person here"
        );
        assert!(rows.iter().all(
            |row| row.app_id == "so.tilo.settings" && row.provider == identity_admin::PROVIDER
        ));

        // Settings reaches the steps as the person using it, without
        // declaring `[actions] invoke`; another app does not.
        let list = RemoteActionMessage::Invoke(request(identity_admin::LIST, None));
        assert_eq!(
            state
                .app_door(&person, "so.tilo.settings", &list)
                .map(|caller| caller.principal().clone())
                .ok(),
            Some(person.principal().clone())
        );
        assert!(state.app_door(&person, "so.tilo.notes", &list).is_err());

        for step in [
            identity_admin::ACTIVATE,
            identity_admin::REMOVE,
            identity_admin::PROTECT,
            identity_admin::REVOKE,
            identity_admin::HOME_PEER_REMOVE,
        ] {
            assert_eq!(
                invoked(&state, &person, &request(step, None)),
                Err(ActionHostError::confirmation_required()),
                "{step} needs the person's confirmation"
            );
        }
        assert!(host.started.lock().unwrap().is_empty(), "nothing started");

        // Listing asks nothing: the person sees their own identities.
        invoked(&state, &person, &request(identity_admin::LIST, None)).expect("listed");
        host.started.lock().unwrap().clear();
        host.answers.lock().unwrap().clear();

        let receipt = invoked(
            &state,
            &person,
            &request(identity_admin::REMOVE, Some(Confirmation::Confirmed)),
        )
        .expect("admitted");
        assert_eq!(
            *host.started.lock().unwrap(),
            [(workspace, owner, identity_admin::REMOVE.to_owned())]
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
            value: Some(serde_json::json!({"removed": "work"})),
            resources: Vec::new(),
        }));
        assert!(matches!(
            state.invocation_status(job),
            Some(tilo_actions::InvocationUpdate::Succeeded { .. })
        ));
    }

    #[test]
    fn identity_admin_is_refused_to_strangers_remote_devices_and_unconfined_callers() {
        let host = Arc::new(Host::default());
        let (state, owner, workspace) = broker(Arc::clone(&host));

        // A person the workspace's policy grants nothing is offered no step that
        // changes the identity, and the broker refuses one before the host
        // hears of it.
        let stranger = Caller::local_person(hex::encode([7; 32])).in_workspace(workspace.clone());
        let offered = state
            .eager_catalogue(&stranger, RowScope::Provider(identity_admin::PROVIDER))
            .0;
        assert!(
            offered.iter().all(|row| row
                .action
                .as_ref()
                .is_some_and(
                    |descriptor| descriptor.contract.risk != tilo_actions::RiskLevel::Privileged
                )),
            "{offered:?}"
        );
        for step in [
            identity_admin::ACTIVATE,
            identity_admin::PROTECT,
            identity_admin::REMOVE,
            identity_admin::REVOKE,
        ] {
            assert!(
                invoked(
                    &state,
                    &stranger,
                    &request(step, Some(Confirmation::Confirmed))
                )
                .is_err(),
                "{step} is refused to a stranger"
            );
        }

        // A remote device, or a caller confined to no workspace, is never offered
        // this machine's identity store.
        let remote = Caller::authenticated([41; 32], [42; 32]).in_workspace(workspace);
        assert!(
            state
                .eager_catalogue(&remote, RowScope::Provider(identity_admin::PROVIDER))
                .0
                .is_empty()
        );
        assert!(invoked(&state, &remote, &request(identity_admin::LIST, None)).is_err());
        let unconfined = Caller::local_person(owner);
        assert!(invoked(&state, &unconfined, &request(identity_admin::LIST, None)).is_err());
        assert!(host.started.lock().unwrap().is_empty(), "nothing started");
    }
}

#[cfg(test)]
mod settings_records_tests {
    use super::super::AppBrokerState;
    use crate::services::{HostStepOutcome, HostStepRequest, HostStepServices};
    use crate::test_support::{
        Grants as _, KindSet, Subject, Workspace, WorkspaceSlot, WorkspaceTree,
    };
    use std::sync::{Arc, Mutex};
    use tilo_action_policy::edits::{FIELD_EDIT_TAG, Weaker, weakening_tag};
    use tilo_actions::remote::{
        ActionHostError, Caller, Confirmation, InvokeArgument, InvokeRequest, RemoteActionTarget,
        settings_records,
    };

    const APP: &str = "so.tilo.settings";
    const RESOURCE: &str = "settings:power";

    /// The session's settings, standing in: one section whose lock delay
    /// weakens the lock when longer or never.
    struct Host {
        minutes: Mutex<u32>,
    }

    impl Host {
        fn descriptor(&self, privileged: bool) -> tilo_actions::ActionDescriptor {
            let now = self.minutes.lock().unwrap().to_string();
            tilo_actions::ActionDescriptor {
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                address: tilo_actions::ActionAddress::new(RESOURCE, "field-edit"),
                resource_kind: "settings.power".into(),
                resource_label: "Power".into(),
                action_label: "Edit".into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                locus: tilo_actions::ExecutionLocus::default(),
                kind: tilo_actions::OperationKind::Document,
                contract: tilo_actions::ActionContract {
                    kind: tilo_actions::ActionKind::Undoable,
                    risk: if privileged {
                        tilo_actions::RiskLevel::Privileged
                    } else {
                        tilo_actions::RiskLevel::Low
                    },
                    tags: vec![
                        FIELD_EDIT_TAG.into(),
                        tilo_actions::ARGUMENT_RISK_TAG.into(),
                        weakening_tag(
                            "lock_minutes",
                            Some(&now),
                            Weaker::Higher { weakest: Some(0) },
                        ),
                    ],
                    ..tilo_actions::ActionContract::default()
                },
                input: tilo_actions::InputSpec::None,
                params: Vec::new(),
                view: None,
            }
        }
    }

    impl HostStepServices for Host {
        fn provider(&self) -> &'static str {
            settings_records::PROVIDER
        }

        fn app_id(&self) -> &'static str {
            APP
        }

        fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
            vec![self.descriptor(false)]
        }

        fn invocation(
            &self,
            address: &tilo_actions::ActionAddress,
            argument: Option<&str>,
        ) -> Option<tilo_actions::ActionDescriptor> {
            let weakens = self
                .descriptor(false)
                .contract
                .confirmation_for_argument(argument)
                != tilo_actions::ConfirmationPolicy::None;
            Some(self.descriptor(weakens)).filter(|descriptor| &descriptor.address == address)
        }

        fn start(
            &self,
            request: &HostStepRequest<'_>,
        ) -> Result<tilo_actions::remote::Pending<HostStepOutcome>, String> {
            let json = request
                .argument
                .and_then(|text| text.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX))
                .ok_or("no edit")?;
            let arguments: serde_json::Value =
                serde_json::from_str(json).map_err(|error| error.to_string())?;
            let minutes = arguments["edit"]["value"].as_u64().ok_or("no value")?;
            *self.minutes.lock().unwrap() = u32::try_from(minutes).map_err(|e| e.to_string())?;
            Ok(tilo_actions::remote::Pending::ready(Ok(
                tilo_actions::ActionOutput::default(),
            )))
        }
    }

    fn lock_after(minutes: u32, confirmation: Option<Confirmation>) -> InvokeRequest {
        InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: RemoteActionTarget {
                provider: settings_records::PROVIDER.into(),
                address: tilo_actions::ActionAddress::new(RESOURCE, "field-edit"),
            },
            argument: InvokeArgument::Json(
                serde_json::from_value(serde_json::json!({
                    "edit": { "path": [{ "field": "lock_minutes" }], "op": "set", "value": minutes }
                }))
                .unwrap(),
            ),
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
    ) -> Result<tilo_actions::remote::InvocationReceipt, ActionHostError> {
        state
            .action_invoke(caller, None, None, request)
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .expect("admission answers at once")
    }

    /// With no window of Settings, the session changes the person's
    /// settings: a change that weakens the lock asks the person first, and
    /// an agent's needs their own answer; a stricter one asks nothing.
    #[test]
    fn a_setting_that_weakens_asks_the_person_and_an_agent_needs_their_answer() {
        let host = Arc::new(Host {
            minutes: Mutex::new(5),
        });
        let owner = hex::encode([41; 32]);
        let mut workspace = Workspace::new(WorkspaceSlot(9), "Settings");
        workspace.set_owner(owner.clone());
        workspace.allow_agent("scribe", KindSet::READ);
        workspace.allow_app(Subject::agent("scribe"), APP, KindSet::ALL);
        let workspace_id = workspace.id().to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let (state, _bridge_rx) = AppBrokerState::test_state(
            Some(tree.shared().authority()),
            crate::services::HostServices {
                host_steps: vec![host.clone()],
                ..Default::default()
            },
        );
        let person = Caller::local_person(owner.clone()).in_workspace(workspace_id.clone());
        let agent = Caller::agent_turn(
            "scribe",
            owner,
            tilo_action_policy::turn::Turn::new("chat/workspace/channels/1", "m1"),
        )
        .in_workspace(workspace_id);
        let minutes = || *host.minutes.lock().unwrap();

        invoked(&state, &person, &lock_after(1, None)).expect("locking sooner asks nothing");
        assert_eq!(minutes(), 1);
        assert_eq!(
            invoked(&state, &person, &lock_after(0, None)),
            Err(ActionHostError::confirmation_required()),
            "never locking asks first"
        );
        assert_eq!(minutes(), 1);
        invoked(
            &state,
            &person,
            &lock_after(10, Some(Confirmation::Confirmed)),
        )
        .expect("the person confirmed it");
        assert_eq!(minutes(), 10);

        invoked(&state, &agent, &lock_after(5, None)).expect("the agent locks sooner");
        assert_eq!(minutes(), 5);
        for confirmation in [None, Some(Confirmation::Confirmed)] {
            assert!(
                invoked(&state, &agent, &lock_after(0, confirmation)).is_err(),
                "an agent's own confirmation is no answer of the person's"
            );
        }
        assert_eq!(minutes(), 5, "nothing weakened");
    }
}
