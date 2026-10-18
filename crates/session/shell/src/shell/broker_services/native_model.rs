//! Owner-private person-approved MODEL selection and exact-byte host dispatch.
//! Provider preparation never creates authority; only the final synchronous
//! enqueue runs under the durable host fence.
use super::NativeOwnerCommitService;
use crate::shell::host_window_contracts;
use agent_provider_config::{ProviderConfig, ProviderProfile};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tilo_action_broker::native_model::{NativeModelHost, NativeModelLease};
use tilo_action_broker::services::{
    HostDispatchModel, NativeModelAuthority, NativeModelContext, NativeModelContract,
    NativeModelGovernorCustody, NativeModelHttpAttempt, TypedNativeModelRequirement,
    WorkspaceTaskExecution,
};
use tilo_joined_room::task_admission::model::NativeModelSource;
use tilo_joined_room::task_admission::model_host::NativeModelHostRegistration;
use tilo_joined_room::task_admission::{
    NativeAdmissionCommand as C, NativeAdmissionReply as R, NativeInvocationPermit,
};
use tilo_joined_room::{NativeCommittedView, NativeWorkspaceOwner};
use tilo_model_gateway::{FrozenNativeAttempt, NativeModelDispatch};

type EnrollmentKey = (uuid::Uuid, uuid::Uuid, u64);
type Enrollments = Arc<Mutex<std::collections::HashMap<EnrollmentKey, Arc<Enrollment>>>>;

pub(super) struct NativeModels {
    owner: Arc<NativeOwnerCommitService>,
    selection_turn: Mutex<()>,
    enrollments: Enrollments,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenRun {
    room: String,
    task: String,
    epoch: u64,
    profile: ProviderProfile,
    model: String,
    contract: NativeModelContract,
}

impl NativeModels {
    pub(super) fn new(owner: Arc<NativeOwnerCommitService>) -> Self {
        Self {
            owner,
            selection_turn: Mutex::new(()),
            enrollments: Arc::default(),
        }
    }

    fn freeze(&self, lease: &dyn NativeModelLease) -> Result<FrozenRun, String> {
        use std::io::Read as _;
        let _turn = self
            .selection_turn
            .lock()
            .map_err(|_| "MODEL selection unavailable")?;
        if !lease.live() {
            return Err("native MODEL delivered lease ended".into());
        }
        // This read authenticates the actual host at the private owner. Neither
        // the task agent nor the inference request supplies a profile/account.
        let contract = self
            .owner
            .read_task_model_contract(lease.room(), lease.binding())?;
        let profile: ProviderProfile = serde_json::from_value(
            contract
                .selection
                .contract
                .get("profile")
                .cloned()
                .ok_or("native MODEL approved profile absent")?,
        )
        .map_err(|_| "native MODEL approved profile invalid")?;
        let model = contract.selection.model.clone();
        if profile.default_model.as_deref() != Some(model.as_str())
            || profile.id != contract.selection.provider
            || !profile.enabled
        {
            return Err("native MODEL approved selection inconsistent".into());
        }
        check_selection(&profile, &model)?;
        let tree = self.owner.tree.read();
        SourceCustody::freeze(&tree, lease, &contract)?;
        let root = tree
            .persist_path
            .as_ref()
            .ok_or("native MODEL private persistence unavailable")?;
        let mut root = root.clone().into_os_string();
        root.push(".native-model");
        let identity = serde_json::to_vec(&(
            lease.room(),
            &lease.binding().task_id,
            lease.binding().epoch,
        ))
        .map_err(|error| error.to_string())?;
        let path = std::path::PathBuf::from(root)
            .join(format!("{}.json", blake3::hash(&identity).to_hex()));
        drop(tree);
        let selected = FrozenRun {
            room: lease.room().into(),
            task: lease.binding().task_id.clone(),
            epoch: lease.binding().epoch,
            profile,
            model,
            contract,
        };
        match std::fs::File::open(&path).and_then(|file| {
            let mut bytes = Vec::new();
            file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
            Ok(bytes)
        }) {
            Ok(bytes) => {
                if bytes.len() > 1024 * 1024 {
                    return Err("native MODEL selection store too large".into());
                }
                let frozen: FrozenRun = serde_json::from_slice(&bytes)
                    .map_err(|_| "native MODEL private selection corrupt")?;
                if serde_json::to_value(&frozen).map_err(|e| e.to_string())?
                    != serde_json::to_value(&selected).map_err(|e| e.to_string())?
                {
                    return Err("native MODEL run selection changed; start a new run".into());
                }
                Ok(frozen)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tilo_files::native::atomic::json::write_private(&path, &selected)
                    .map_err(|error| error.to_string())?;
                Ok(selected)
            }
            Err(error) => Err(error.to_string()),
        }
    }
}

fn live_config() -> Result<ProviderConfig, String> {
    let source =
        settings_store::installed_source().ok_or("native MODEL live Settings unavailable")?;
    let document = source
        .load_committed()
        .ok_or("native MODEL committed Settings unavailable")?;
    config_from_document(&document)
}

fn config_from_document(
    document: &serde_json::Map<String, serde_json::Value>,
) -> Result<ProviderConfig, String> {
    let section = document
        .get(agent_provider_config::SETTINGS_SECTION)
        .and_then(serde_json::Value::as_object)
        .ok_or("native MODEL provider settings unavailable")?;
    let local = serde_json::from_value(
        section
            .get("config")
            .cloned()
            .ok_or("native MODEL provider settings absent")?,
    )
    .map_err(|_| "native MODEL provider settings invalid")?;
    let hosted = section
        .get("hosted")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| "native MODEL hosted settings invalid")?
        .unwrap_or_default();
    Ok(ProviderConfig::from_stored(local, hosted).sanitised())
}

fn check_selection(profile: &ProviderProfile, model: &str) -> Result<(), String> {
    check_config(&live_config()?, profile, model)
}

fn check_config(
    config: &ProviderConfig,
    profile: &ProviderProfile,
    model: &str,
) -> Result<(), String> {
    if !config.validation_errors().is_empty()
        || config.active_profile() != Some(profile)
        || profile.default_model.as_deref() != Some(model)
    {
        return Err("native MODEL live provider/account selection changed".into());
    }
    Ok(())
}

/// Resolves only the already-approved source: attached resources never acquire
/// window authority and a windowless task never borrows a foreground window.
struct SourceCustody;
impl SourceCustody {
    fn freeze(
        tree: &crate::shell::state::WorkspaceTree,
        lease: &dyn NativeModelLease,
        contract: &NativeModelContract,
    ) -> Result<Self, String> {
        let workspace = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == lease.room())
            .ok_or("native MODEL workspace unavailable")?;
        if !workspace
            .tasks
            .iter()
            .any(|task| task.id == lease.binding().task_id)
        {
            return Err("native MODEL actual task unavailable".into());
        }
        match contract.source_for(lease.room(), &lease.binding().task_id)? {
            NativeModelSource::Task { .. } => {}
            NativeModelSource::Window { window } => {
                let origin: host_window_contracts::HostWindowContract = serde_json::from_value(
                    contract
                        .selection
                        .contract
                        .get("origin")
                        .cloned()
                        .ok_or("native MODEL approved window origin absent")?,
                )
                .map_err(|_| "native MODEL approved window origin invalid")?;
                if origin.window != window
                    || origin.request_model != contract.request_model
                    || origin.kind != contract.kind
                    || origin.room != lease.room()
                    || !host_window_contracts::published(tree).contains(&origin)
                {
                    return Err("native MODEL originating window changed".into());
                }
            }
        }
        Ok(Self)
    }
}

impl NativeModelHost for NativeModels {
    fn claim(
        &self,
        lease: Arc<dyn NativeModelLease>,
        invocation: uuid::Uuid,
    ) -> Result<Arc<dyn NativeModelDispatch>, String> {
        let frozen = self.freeze(lease.as_ref())?;
        // Fetch before synchronous owner turns, never from a public replica.
        let private_run_digest = self
            .owner
            .private_run_digest(lease.room(), lease.binding())?;
        Ok(Arc::new(Dispatch {
            owner: self.owner.clone(),
            lease,
            frozen,
            invocation,
            private_run_digest,
            enrollments: self.enrollments.clone(),
            prepared: Mutex::new(None),
            dispatched: Mutex::new(None),
        }))
    }
}
struct Prepared {
    owner: NativeWorkspaceOwner,
    requirement: TypedNativeModelRequirement,
    permit: Option<NativeInvocationPermit>,
    governor: tilo_user_identity::DeviceCert,
    endpoint: String,
    registration: Option<Arc<Enrollment>>,
}
// This gate is shared with the real lease-close listener and pinned only
// through final synchronous enqueue, never DNS, governor RPC or HTTP response.
struct Enrollment {
    state: Mutex<EnrollmentState>,
}
struct EnrollmentState {
    live: bool,
    registration: NativeModelHostRegistration,
}
struct Dispatch {
    owner: Arc<NativeOwnerCommitService>,
    lease: Arc<dyn NativeModelLease>,
    frozen: FrozenRun,
    invocation: uuid::Uuid,
    private_run_digest: [u8; 32],
    enrollments: Enrollments,
    prepared: Mutex<Option<Prepared>>,
    dispatched: Mutex<Option<Prepared>>,
}
struct Authority<'a> {
    dispatch: &'a Dispatch,
}
impl NativeModelAuthority for Authority<'_> {
    fn with_current_model_context(
        &self,
        view: &NativeCommittedView,
        now: SystemTime,
        enqueue: &mut dyn FnMut(NativeModelContext) -> Result<(), String>,
    ) -> Result<(), String> {
        let dispatch = self.dispatch;
        dispatch.lease.with_current(
            view,
            dispatch.frozen.contract.clone(),
            now,
            &mut |mut context| {
                // Global order: owner -> connection/lease -> source -> Settings.
                // Publication has completed: no owner read or publication under these gates.
                let tree = dispatch.owner.tree.read();
                SourceCustody::freeze(&tree, dispatch.lease.as_ref(), &dispatch.frozen.contract)?;
                settings_store::with_installed_committed(&mut |document| {
                    check_config(
                        &config_from_document(document)?,
                        &dispatch.frozen.profile,
                        &dispatch.frozen.model,
                    )?;
                    if view.device() != view.governor() {
                        context.private_run_digest = dispatch.private_run_digest;
                    }
                    if context.private_run_digest != dispatch.private_run_digest {
                        return Err("native MODEL private run changed".into());
                    }
                    enqueue(context.clone())
                })
            },
        )
    }
    fn current_model_context(
        &self,
        view: &NativeCommittedView,
        now: SystemTime,
    ) -> Result<NativeModelContext, String> {
        check_selection(&self.dispatch.frozen.profile, &self.dispatch.frozen.model)?;
        let mut context =
            self.dispatch
                .lease
                .current(view, self.dispatch.frozen.contract.clone(), now)?;
        if view.device() != view.governor() {
            context.private_run_digest = self.dispatch.private_run_digest;
        }
        if context.private_run_digest != self.dispatch.private_run_digest {
            return Err("native MODEL private run changed".into());
        }
        Ok(context)
    }
}

impl Dispatch {
    fn check_invocation(&self, invocation: &serde_json::Value) -> Result<(), String> {
        let id: uuid::Uuid = serde_json::from_value(invocation.clone())
            .map_err(|_| "invalid native MODEL invocation")?;
        if id != self.invocation {
            return Err("native MODEL invocation changed".into());
        }
        Ok(())
    }

    fn prepare_attempt(
        &self,
        invocation: &serde_json::Value,
        attempt: &FrozenNativeAttempt,
    ) -> Result<(), String> {
        self.check_invocation(invocation)?;
        tilo_action_broker::native_model::attempt_matches(
            attempt,
            &self.frozen.profile,
            &self.frozen.model,
        )?;
        let (owner, room) = self.owner.task_execution_owner(self.lease.room())?;
        let (host, governor) = owner.with_current(room, |view| {
            Ok((view.device().clone(), view.governor().clone()))
        })?;
        let (key, signer) = tilo_identity_host::broker::trusted_current_device(self.lease.room())
            .map_err(|error| error.to_string())?;
        if signer != host {
            return Err("native MODEL physical host signer changed".into());
        }
        {
            let tree = self.owner.tree.read();
            SourceCustody::freeze(&tree, self.lease.as_ref(), &self.frozen.contract)?;
        }
        let authority = Authority { dispatch: self };
        let context = owner.with_current(room, |view| {
            authority.current_model_context(view, SystemTime::now())
        })?;
        let requirement = TypedNativeModelRequirement::freeze(
            TypedNativeModelRequirement {
                room,
                run: self.lease.binding().clone(),
                invocation: self.invocation,
                request: uuid::Uuid::new_v4(),
                host: host.device_public_key,
                private_run_digest: context.private_run_digest,
                modelselection: self.frozen.contract.clone(),
                requestdigest: [0; 32],
            },
            &attempt.body,
        )?;
        // Avoid all network waits under tree/owner custody. The live source is
        // checked again and pinned at the actual synchronous HTTP enqueue.
        let registration = if host == governor {
            None
        } else {
            Some(NativeModelHostRegistration::capture(
                &owner,
                room,
                self.lease
                    .connection()
                    .ok_or("native MODEL protected connection absent")?,
                &authority,
                &key,
            )?)
        };
        let registration = registration
            .map(|registration| self.enroll(&registration, &requirement))
            .transpose()?;
        let permit = registration
            .as_ref()
            .map(|registration| self.remote_permit(registration, &requirement))
            .transpose()?;
        let mut prepared = self
            .prepared
            .lock()
            .map_err(|_| "native MODEL preparation unavailable")?;
        if prepared.is_some() {
            return Err("native MODEL attempt already prepared".into());
        }
        *prepared = Some(Prepared {
            owner,
            requirement,
            permit,
            governor,
            endpoint: attempt.endpoint.clone(),
            registration,
        });
        drop(prepared);
        Ok(())
    }

    fn enroll(
        &self,
        registration: &NativeModelHostRegistration,
        requirement: &TypedNativeModelRequirement,
    ) -> Result<Arc<Enrollment>, String> {
        let connection = self
            .lease
            .connection()
            .ok_or("native MODEL connection absent")?;
        let identity = (
            connection.connection,
            connection.lease,
            requirement.run.revision,
        );
        let mut enrollments = self
            .enrollments
            .lock()
            .map_err(|_| "native MODEL enrollments unavailable")?;
        if let Some(enrollment) = enrollments.get(&identity) {
            let state = enrollment
                .state
                .lock()
                .map_err(|_| "native MODEL enrollment unavailable")?;
            if !state.live {
                return Err("native MODEL enrollment revoked".into());
            }
            if state.registration.context() != registration.context() {
                return Err("native MODEL enrollment context changed".into());
            }
            // Keep one enrollment across sequential and concurrent inference.
            if SystemTime::now() < state.registration.expires_at() {
                return Ok(enrollment.clone());
            }
        }
        // Expired registrations cannot dispatch. A newly signed live capture
        // renews the same delivered lease, never an old permit or request.
        let enrollment = Arc::new(Enrollment {
            state: Mutex::new(EnrollmentState {
                live: true,
                registration: registration.clone(),
            }),
        });
        let listener = enrollment.clone();
        let service = self.owner.clone();
        let room = requirement.room;
        self.lease.on_close(Box::new(move || {
            let id = {
                let mut state = listener
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.live = false;
                state.registration.id()
            };
            let _ = std::thread::Builder::new()
                .name("model-host-revoke".into())
                .spawn(move || {
                    let _ =
                        service.native_task_custody(room, &C::RevokeModelHost { registration: id });
                });
        }))?;
        if !self.lease.live() {
            return Err("native MODEL lease closed before enrollment".into());
        }
        self.owner.receive_task_fences(room)?;
        if !matches!(
            self.owner
                .native_task_custody(room, &C::RegisterModelHost(registration.clone()))?,
            R::Acknowledged
        ) {
            return Err("unexpected MODEL enrollment reply".into());
        }
        if !self.lease.live() {
            let _ = self.owner.native_task_custody(
                room,
                &C::RevokeModelHost {
                    registration: registration.id(),
                },
            );
            return Err("native MODEL lease closed during enrollment".into());
        }
        enrollments.insert(identity, enrollment.clone());
        drop(enrollments);
        Ok(enrollment)
    }

    fn remote_permit(
        &self,
        enrollment: &Enrollment,
        requirement: &TypedNativeModelRequirement,
    ) -> Result<NativeInvocationPermit, String> {
        let room = requirement.room;
        let id = {
            let state = enrollment
                .state
                .lock()
                .map_err(|_| "native MODEL enrollment unavailable")?;
            if !state.live {
                return Err("native MODEL enrollment revoked".into());
            }
            state.registration.id()
        };
        let R::Reserved(reservation) = self.owner.native_task_custody(
            room,
            &C::ReserveModel {
                registration: id,
                requirement: requirement.clone(),
            },
        )?
        else {
            return Err("unexpected MODEL reservation reply".into());
        };
        let R::Claimed(permit) = self.owner.native_task_custody(
            room,
            &C::ClaimModel {
                registration: id,
                requirement: requirement.clone(),
                reservation,
            },
        )?
        else {
            return Err("unexpected MODEL permit reply".into());
        };
        self.owner.receive_task_fences(room)?;
        Ok(permit)
    }
}
impl NativeModelDispatch for Dispatch {
    fn completed(
        &self,
        proof: &tilo_model_gateway::native_delivery::NativeModelSuccess,
    ) -> Result<(), String> {
        let prepared = self
            .dispatched
            .lock()
            .map_err(|_| "MODEL dispatch unavailable")?
            .take()
            .ok_or("MODEL successful dispatch absent")?;
        let (key, signer) = tilo_identity_host::broker::trusted_current_device(self.lease.room())
            .map_err(|e| e.to_string())?;
        if signer.device_public_key != prepared.requirement.host {
            return Err("MODEL delivery physical host signer changed".into());
        }
        let receipt =
            tilo_joined_room::task_admission::model_delivery::NativeModelDeliveryReceipt::capture(
                proof,
                &prepared.requirement,
                prepared
                    .permit
                    .as_ref()
                    .ok_or("MODEL consumed permit absent")?,
                &key,
            )?;
        if signer == prepared.governor {
            prepared.owner.record_native_model_delivery(&receipt)
        } else {
            match self
                .owner
                .native_task_custody(receipt.room(), &C::ModelDelivery(receipt))?
            {
                R::Acknowledged => Ok(()),
                _ => Err("unexpected MODEL delivery reply".into()),
            }
        }
    }
    fn prepare<'a>(
        &'a self,
        invocation: &'a serde_json::Value,
        attempt: &'a FrozenNativeAttempt,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move { self.prepare_attempt(invocation, attempt) })
    }
    fn dispatch(
        &self,
        invocation: &serde_json::Value,
        attempt: &FrozenNativeAttempt,
        start: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.check_invocation(invocation)?;
        tilo_action_broker::native_model::attempt_matches(
            attempt,
            &self.frozen.profile,
            &self.frozen.model,
        )?;
        let mut prepared = self
            .prepared
            .lock()
            .map_err(|_| "native MODEL preparation unavailable")?
            .take()
            .ok_or("native MODEL attempt absent or consumed")?;
        if prepared.endpoint != attempt.endpoint {
            return Err("native MODEL final endpoint changed".into());
        }
        {
            let tree = self.owner.tree.read();
            SourceCustody::freeze(&tree, self.lease.as_ref(), &self.frozen.contract)?;
        }
        let enrollment = prepared
            .registration
            .as_ref()
            .map(|registration| {
                registration
                    .state
                    .lock()
                    .map_err(|_| "native MODEL enrollment unavailable")
            })
            .transpose()?;
        if let Some(state) = &enrollment
            && (!state.live || SystemTime::now() >= state.registration.expires_at())
        {
            return Err("native MODEL host enrollment expired or revoked".into());
        }
        let authority = Authority { dispatch: self };
        let gate = HostDispatchModel {
            owner: &prepared.owner,
            authority: &authority,
        };
        // Admission publishes through this tree's writer. Pin the live source
        // only after that publication, inside the final synchronous enqueue,
        // or local governor dispatch deadlocks its own publication writer.
        let enqueue = |_: &[u8]| {
            if let Some(state) = &enrollment
                && (!state.live || SystemTime::now() >= state.registration.expires_at())
            {
                return Err("native MODEL host enrollment expired or revoked".into());
            }
            start()
        };
        let result = if let Some(permit) = &prepared.permit {
            gate.dispatch(
                &prepared.requirement,
                permit,
                NativeModelHttpAttempt {
                    governor: &prepared.governor,
                    outbound_body: &attempt.body,
                },
                enqueue,
            )
        } else {
            let (key, host) = tilo_identity_host::broker::trusted_current_device(self.lease.room())
                .map_err(|error| error.to_string())?;
            gate.dispatch_local(
                &prepared.requirement,
                NativeModelGovernorCustody {
                    host: &host,
                    key: &key,
                    now: SystemTime::now(),
                },
                &attempt.body,
                enqueue,
            )
            .map(|((), permit)| {
                prepared.permit = Some(permit);
            })
        };
        drop(enrollment);
        result?;
        *self
            .dispatched
            .lock()
            .map_err(|_| "MODEL dispatch unavailable")? = Some(prepared);
        Ok(())
    }
}
