//! Steps the host itself runs for the person at this device: pairing a
//! nearby device, and administering the person's identities and the devices
//! trusted under them.
//!
//! Each is answered under the host's own provider, offered only to a caller
//! on this machine confined to a room, and admitted against that room's
//! policy under the service's app id — the application whose controls reach
//! the steps as the person using it. The door treats them alike; only the
//! service that runs a step differs.

use std::sync::Arc;

use tilo_actions::remote::{Pending, device_pairing, identity_admin};
use tilo_actions::{ActionAddress, ActionDescriptor};

use super::{BrokerActor, CapnpBrokerState};
use crate::services::{
    DevicePairingOutcome, DevicePairingRequest, DevicePairingServices, HostServices,
    IdentityAdminRequest, IdentityAdminServices,
};
use tilo_actions::remote::{
    ActionHostError, Caller, InvocationReceipt, InvokeRequest, Reservation,
};

/// One installed host-step service.
#[derive(Clone)]
pub(super) enum HostSteps {
    Pairing(Arc<dyn DevicePairingServices>),
    Identity(Arc<dyn IdentityAdminServices>),
}

impl HostSteps {
    /// Every host-step service `services` installs.
    pub(super) fn installed(services: &HostServices) -> Vec<Self> {
        let pairing = services.device_pairing.clone().map(Self::Pairing);
        let identity = services.identity_admin.clone().map(Self::Identity);
        pairing.into_iter().chain(identity).collect()
    }

    /// The installed service that answers under `provider`, if any.
    pub(super) fn for_provider(services: &HostServices, provider: &str) -> Option<Self> {
        Self::installed(services)
            .into_iter()
            .find(|steps| steps.provider() == provider)
    }

    /// The provider the steps are answered under.
    pub(super) fn provider(&self) -> &'static str {
        match self {
            Self::Pairing(_) => device_pairing::PROVIDER,
            Self::Identity(_) => identity_admin::PROVIDER,
        }
    }

    /// The application whose controls reach the steps as their person.
    pub(super) fn app_id(&self) -> &str {
        match self {
            Self::Pairing(service) => service.app_id(),
            Self::Identity(service) => service.app_id(),
        }
    }

    /// Every step, described.
    pub(super) fn descriptors(&self) -> Vec<ActionDescriptor> {
        match self {
            Self::Pairing(service) => service.descriptors(),
            Self::Identity(service) => service.descriptors(),
        }
    }

    /// Start the admitted step `address` for `actor` in `room`.
    ///
    /// # Errors
    ///
    /// Why the host refuses to start it.
    pub(super) fn start(
        &self,
        room: &str,
        actor: &BrokerActor,
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Result<Pending<DevicePairingOutcome>, String> {
        match self {
            Self::Pairing(service) => service.start(&DevicePairingRequest {
                room,
                actor,
                address,
                argument,
            }),
            Self::Identity(service) => service.start(&IdentityAdminRequest {
                room,
                actor,
                address,
                argument,
            }),
        }
    }
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
    original.network.is_none()
        && !original.presentation.is_empty()
        && caller.room() == Some(original.room.as_str())
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

impl CapnpBrokerState {
    /// Admit one host step (pairing a nearby device, administering the
    /// person's identity) for `caller` and start it on the host.
    ///
    /// Host steps run on this machine, for the person at it: a remote device
    /// is never offered them, and the caller must be confined to the room its
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
        steps: &super::host_steps::HostSteps,
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
        if !is_local_host_caller(caller, origin) {
            return Err(abandon(ActionHostError::UnknownAction));
        }
        let Some(room) = caller.room() else {
            return Err(abandon(ActionHostError::NotPermitted));
        };
        let address = &request.target.address;
        let Some(descriptor) = steps
            .descriptors()
            .into_iter()
            .find(|descriptor| &descriptor.address == address)
        else {
            return Err(abandon(ActionHostError::UnknownAction));
        };
        let (workspace, policy, now) = self
            .current_framework_authority(caller, origin, room)
            .map_err(abandon)?;
        let argument = request.argument.to_slot();
        // One answer covers both gates of this exact host invocation.
        let witnessed = self.consume_answer(
            request.confirmation.as_ref(),
            &self.witness_binding(policy.id(), room, request),
        );
        self.framework_policy(
            (&workspace, &policy, now),
            (steps.app_id(), &descriptor),
            request.confirmation.as_ref(),
            witnessed,
            false,
        )
        .map_err(abandon)?;
        if !Self::confirmed_as_asked(
            &policy,
            &descriptor,
            argument.as_deref(),
            request.confirmation.as_ref(),
            witnessed,
        ) {
            return Err(abandon(ActionHostError::ConfirmationRequired));
        }
        let admitted = self
            .admit_host_action(
                caller,
                origin,
                super::host_action_admission::HostActionAdmission {
                    room,
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
                self.current_framework_authority(caller, origin, room)?;
            if !Self::confirmed_as_asked(
                &policy,
                &descriptor,
                argument.as_deref(),
                request.confirmation.as_ref(),
                witnessed,
            ) {
                return Err(ActionHostError::ConfirmationRequired);
            }
            self.admit_answer(
                (&workspace, &policy, now),
                steps.app_id(),
                &descriptor,
                (argument.as_deref(), request.confirmation.as_ref()),
                witnessed,
            )?;
            Ok(policy)
        })?;
        let started = steps.start(room, &policy, address, argument.as_deref());
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
        started: Pending<DevicePairingOutcome>,
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
pub(super) mod signed_test_support {
    use super::super::{
        BrokerChannels, CapnpBrokerState, DurableInvocation, FanoutState, InvocationKey,
        PeerPublisher, principal_id_for_root, published_invocation::InvocationCaller,
    };
    use crate::services::HostServices;
    use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
    use ed25519_dalek::Signer as _;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use tilo_actions::remote::{ActionHostError, Caller, InvocationReceipt, InvokeRequest};

    struct NativeOwnerReceipts {
        owner: Arc<tilo_joined_room::NativeWorkspaceOwner>,
        key: ed25519_dalek::SigningKey,
        certificate: tilo_user_identity::DeviceCert,
    }

    impl tilo_owner_commit::OwnerCommitService for NativeOwnerReceipts {
        fn health(&self) -> Result<(), String> {
            let room = tilo_desktop_model::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
            self.owner.with_current(room, |_| Ok(()))
        }

        fn commit(
            &self,
            request: tilo_owner_commit::OwnerCommitRequest,
        ) -> tilo_owner_commit::CommitFuture {
            let owner = self.owner.clone();
            let key = self.key.clone();
            let certificate = self.certificate.clone();
            Box::pin(async move {
                owner.commit_effect_receipt(
                    &request,
                    &certificate,
                    &key,
                    std::time::SystemTime::now(),
                )
            })
        }
    }

    pub(crate) struct Authority {
        certificate: tilo_user_identity::DeviceCert,
        pub revoked: AtomicBool,
        pub device_governs: AtomicBool,
        time: Mutex<crate::peer_actions::PolicyTime>,
        replace_time_on_authorize: Mutex<Option<crate::peer_actions::PolicyTime>>,
    }

    impl Authority {
        pub fn replace_time_on_authorize(&self, time: crate::peer_actions::PolicyTime) {
            *self.replace_time_on_authorize.lock().unwrap() = Some(time);
        }
    }

    impl crate::peer_actions::CertificateAuthority for Authority {
        fn governs(&self, _: uuid::Uuid, device: Option<&[u8; 32]>) -> bool {
            device.is_none() || self.device_governs.load(Ordering::SeqCst)
        }

        fn policy_time(&self) -> Result<Option<crate::peer_actions::PolicyTime>, String> {
            Ok(Some(self.time.lock().unwrap().clone()))
        }

        fn authorize(
            &self,
            _room: uuid::Uuid,
            certificate: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            if let Some(time) = self.replace_time_on_authorize.lock().unwrap().take() {
                *self.time.lock().unwrap() = time;
            }
            if self.revoked.load(Ordering::SeqCst) || certificate != &self.certificate {
                return Err("device is no longer enrolled".into());
            }
            tilo_user_identity::verify_device_cert(certificate).map_err(|error| error.to_string())
        }
    }

    pub(crate) struct Fixture {
        pub state: Arc<CapnpBrokerState>,
        pub caller: Caller,
        pub origin: InvocationCaller,
        pub authority: Arc<Authority>,
        pub room: String,
        pub tree: crate::test_support::WorkspaceTreeHandle,
        certified_host: Option<Arc<dyn tilo_actions::remote::ActionHost>>,
        _fanout: Arc<FanoutState>,
        _environment: tilo_test_env::ScopedEnv,
        _directory: tempfile::TempDir,
        clock: crate::peer_actions::OwnerPolicyClock,
        _origin: crate::peer_actions::PolicyOrigin,
    }

    impl Fixture {
        pub fn new_policy_origin(&self) -> crate::peer_actions::PolicyOrigin {
            self.clock.new_origin()
        }

        pub fn retire_policy_origin(&self) {
            self._origin.invalidate();
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
            let mut workspace = Workspace::new(WorkspaceId(9), "Host actions");
            workspace.set_room(
                tilo_desktop_model::RoomId::parse(tilo_joined_room::testing::ROOM).unwrap(),
            );
            let room = workspace.room.to_string();
            let (key, certificate) =
                tilo_identity_host::broker::trusted_current_device(&room).unwrap();
            let owner = Arc::new(
                tilo_joined_room::open_file(&directory.path().join("native.json")).unwrap(),
            );
            tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &certificate)
                .unwrap();
            let publisher = PeerPublisher::with_optional_owner_commit(
                None,
                Arc::new(NativeOwnerReceipts {
                    owner,
                    key: key.clone(),
                    certificate: certificate.clone(),
                }),
            );
            let person = principal_id_for_root(certificate.user_public_key);
            workspace.set_owner(person.clone());
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(workspace);
            let tree = tree.shared();
            let clock = crate::peer_actions::OwnerPolicyClock::new().unwrap();
            let policy_origin = clock.new_origin();
            let authority = Arc::new(Authority {
                certificate: certificate.clone(),
                revoked: AtomicBool::new(false),
                device_governs: AtomicBool::new(true),
                time: Mutex::new(policy_origin.capture().unwrap()),
                replace_time_on_authorize: Mutex::new(None),
            });
            let config = crate::peer_actions::NativePeerBrokerConfig {
                journal_root: directory.path().join("journal"),
                certificate_authority: authority.clone(),
            };
            let fanout = Arc::new(FanoutState::new());
            let (state, _) = CapnpBrokerState::new_configured(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                publisher,
                Some(tree.authority()),
                None,
                Some(&config),
                services,
            );
            let presentation = runtime_collab::CertifiedJoinIdentity {
                workspace_id: room.clone(),
                device_certificate: serde_json::to_vec(&certificate).unwrap(),
                proof: key
                    .sign(&runtime_identity::collaboration_join_signing_bytes(&room))
                    .to_bytes()
                    .to_vec(),
            }
            .encode()
            .unwrap();
            let verified = state.verify_action_caller(&room, &presentation).unwrap();
            let origin = InvocationCaller::Local {
                route: super::super::CrdtRoute::cluster(&room),
                admitted: Box::new(verified.admitted),
                time: state.registered_policy_time().unwrap(),
            };
            Self {
                state,
                caller: Caller::local_person(person).in_room(room.clone()),
                origin,
                authority,
                room,
                tree,
                certified_host: None,
                _fanout: fanout,
                _environment: environment,
                _directory: directory,
                clock,
                _origin: policy_origin,
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
                tilo_identity_host::broker::trusted_current_device(&fixture.room).unwrap();
            let presentation = runtime_collab::CertifiedJoinIdentity {
                workspace_id: fixture.room.clone(),
                device_certificate: serde_json::to_vec(&certificate).unwrap(),
                proof: key
                    .sign(&runtime_identity::collaboration_join_signing_bytes(
                        &fixture.room,
                    ))
                    .to_bytes()
                    .to_vec(),
            }
            .encode()
            .unwrap();
            fixture.caller =
                Caller::authenticated(certificate.user_public_key, certificate.device_public_key)
                    .in_room(&fixture.room)
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
                        .verify_action_caller(&fixture.room, &presentation)
                        .unwrap(),
                ),
                foreign_restriction: foreign,
            };
            fixture
        }

        fn owner(&self) -> super::super::InvocationOwner {
            match &self.origin {
                InvocationCaller::Certified { caller, .. } => caller.owner(),
                InvocationCaller::Local { admitted, .. } => {
                    let certificate = admitted.certificate.as_ref().unwrap();
                    super::super::InvocationOwner::AuthenticatedLocal {
                        root: certificate.user_public_key,
                        device: certificate.device_public_key,
                        agent: None,
                    }
                }
                _ => panic!("authenticated custody"),
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
                .wait_timeout(std::time::Duration::from_secs(5))
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
                .wait_timeout(std::time::Duration::from_secs(5))
                .ok()
                .flatten()
                .expect("catalogue answers immediately")
                .expect("the certified caller may query")
        }

        pub fn admission_check(&self, request: &InvokeRequest) -> Box<dyn Fn() + Send> {
            let key = InvocationKey {
                room: self.room.clone(),
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

        pub fn record(&self, request: &InvokeRequest) -> DurableInvocation {
            let guard = self.state.journal.lock().unwrap();
            let journal = guard.as_ref().unwrap().as_ref().unwrap();
            journal
                .lookup(&InvocationKey {
                    room: self.room.clone(),
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
                super::super::InvocationJournal::open(&self._directory.path().join("journal"))
                    .unwrap(),
            ));
        }

        pub fn assert_signed(&self, request: &InvokeRequest, terminal: bool) -> DurableInvocation {
            let record = self.record(request);
            let admission = record.admission.as_ref().expect("signed admission");
            let receipt = admission.public_receipt();
            crate::action_evidence::verify(receipt, &receipt.facts).unwrap();
            assert_eq!(receipt.facts.invocation, request.invocation);
            assert_eq!(receipt.facts.room, self.room);
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
    use super::super::action_host::RowScope;
    use super::super::{BrokerChannels, CapnpBrokerState, FanoutState, PeerPublisher};
    use crate::services::{IdentityAdminOutcome, IdentityAdminRequest, IdentityAdminServices};
    use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
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
        answers: Mutex<Vec<Completer<IdentityAdminOutcome>>>,
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

    impl IdentityAdminServices for Host {
        fn app_id(&self) -> &'static str {
            "so.tilo.settings"
        }

        fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
            identity_admin::STEPS
                .into_iter()
                .map(|step| tilo_actions::ActionDescriptor {
                    semantic_identity: None,
                    address: tilo_actions::ActionAddress::new(identity_admin::RESOURCE, step),
                    resource_kind: identity_admin::RESOURCE.into(),
                    resource_label: "Identities".into(),
                    action_label: step.into(),
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

        fn start(
            &self,
            request: &IdentityAdminRequest<'_>,
        ) -> Result<Pending<IdentityAdminOutcome>, String> {
            if let Some(check) = self.admission_check.lock().unwrap().as_ref() {
                check();
            }
            self.started.lock().unwrap().push((
                request.room.to_owned(),
                request.actor.id().to_owned(),
                request.address.action.clone(),
            ));
            let (answer, pending) = pending();
            self.answers.lock().unwrap().push(answer);
            Ok(pending)
        }
    }

    fn broker(host: Arc<Host>) -> (Arc<CapnpBrokerState>, String, String) {
        let owner = hex::encode([41; 32]);
        let mut workspace = Workspace::new(WorkspaceId(9), "Identities");
        workspace.set_owner(owner.clone());
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
                identity_admin: Some(host),
                ..Default::default()
            },
        );
        (state, owner, room)
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
        state: &Arc<CapnpBrokerState>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::InvocationReceipt, ActionHostError> {
        state
            .action_invoke(caller, None, None, request)
            .wait_timeout(std::time::Duration::from_secs(5))
            .ok()
            .flatten()
            .expect("admission answers at once")
    }

    #[derive(Default)]
    struct PairingHost(Host);

    impl crate::services::DevicePairingServices for PairingHost {
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

        fn start(
            &self,
            request: &crate::services::DevicePairingRequest<'_>,
        ) -> Result<Pending<crate::services::DevicePairingOutcome>, String> {
            self.0.start(&IdentityAdminRequest {
                room: request.room,
                actor: request.actor,
                address: request.address,
                argument: request.argument,
            })
        }
    }

    #[test]
    fn certified_host_steps_admit_only_nonforeign_local_custody() {
        for pairing in [false, true] {
            for foreign in [false, true] {
                let host = Arc::new(PairingHost::default());
                let identity = Arc::new(Host::default());
                let services = crate::services::HostServices {
                    device_pairing: pairing
                        .then(|| host.clone() as Arc<dyn crate::services::DevicePairingServices>),
                    identity_admin: (!pairing)
                        .then(|| identity.clone() as Arc<dyn IdentityAdminServices>),
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
                    .complete(Ok(Default::default()));
                f.assert_signed(&asked, true);
                f.invoke(&asked).unwrap();
                assert_eq!(service.started.lock().unwrap().len(), 1);
                f.authority
                    .revoked
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                asked.invocation = tilo_actions::InvocationId::new();
                assert!(f.invoke(&asked).is_err());
                assert_eq!(service.started.lock().unwrap().len(), 1);
            }
        }
    }

    #[test]
    fn device_pairing_uses_the_same_signed_admission_and_terminal_path() {
        let host = Arc::new(PairingHost::default());
        let f = super::signed_test_support::Fixture::new(crate::services::HostServices {
            device_pairing: Some(host.clone()),
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

    #[test]
    fn identity_admin_signs_before_start_and_retains_the_original_uuid() {
        let host = Arc::new(Host::default());
        let f = super::signed_test_support::Fixture::new(crate::services::HostServices {
            identity_admin: Some(host.clone()),
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

    #[test]
    fn identity_admin_persists_failed_and_dropped_host_outcomes() {
        for dropped in [false, true] {
            let host = Arc::new(Host::default());
            let f = super::signed_test_support::Fixture::new(crate::services::HostServices {
                identity_admin: Some(host.clone()),
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

    #[test]
    fn identity_admin_admission_and_completion_barriers_are_fail_closed() {
        for admission_failure in [false, true] {
            let host = Arc::new(Host::default());
            let f = super::signed_test_support::Fixture::new(crate::services::HostServices {
                identity_admin: Some(host.clone()),
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

    #[test]
    fn host_steps_recheck_live_authority_after_blocked_durable_admission() {
        use std::sync::{atomic::Ordering, mpsc};
        use std::time::Duration;
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
                    identity_admin: Some(host.clone()),
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
                    resume.recv_timeout(Duration::from_secs(5)).unwrap();
                }));
                let result = std::thread::scope(|scope| {
                    let state = f.state.clone();
                    let caller = f.caller.clone();
                    let origin = f.origin.clone();
                    let steps = super::HostSteps::Identity(host.clone());
                    let asked = &asked;
                    let worker = scope.spawn(move || {
                        state.invoke_host_step(&caller, Some(&origin), &steps, asked)
                    });
                    blocked.recv_timeout(Duration::from_secs(5)).unwrap();
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

    #[test]
    fn identity_admin_refuses_revoked_custody_and_missing_signer_before_start() {
        for revoked in [false, true] {
            let host = Arc::new(Host::default());
            let f = super::signed_test_support::Fixture::new(crate::services::HostServices {
                identity_admin: Some(host.clone()),
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
        let (state, owner, room) = broker(Arc::clone(&host));
        let person = Caller::local_person(owner.clone()).in_room(room.clone());

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
                Err(ActionHostError::ConfirmationRequired),
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
            [(room, owner, identity_admin::REMOVE.to_owned())]
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
        let (state, owner, room) = broker(Arc::clone(&host));

        // A person the room's policy grants nothing is offered no step that
        // changes the identity, and the broker refuses one before the host
        // hears of it.
        let stranger = Caller::local_person(hex::encode([7; 32])).in_room(room.clone());
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

        // A remote device, or a caller confined to no room, is never offered
        // this machine's identity store.
        let remote = Caller::authenticated([41; 32], [42; 32]).in_room(room);
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
