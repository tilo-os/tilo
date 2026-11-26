//! The device action service: what another device of a workspace asks this
//! device directly, with no member session in between.
//!
//! Every workspace this device holds answers the devices' action service
//! (`tilo-actions/1`) as any member device does, checked against the
//! workspace's members as they stand. Two things are answered here. A device
//! asks this one to take a turn of work (`tilo.work#take`,
//! [`crate::work`]): it does when [`takes_turn`](crate::work::takes_turn) says
//! it is this device's turn, runs it through the [`HostJobs`] its embedder
//! installed, and answers with what it produced. And a workspace's governance
//! actions (inviting someone, listing and cancelling invitations) are decided
//! as the workspace's governance decides them for the asking device's
//! certificate. A run on a workspace's Linux machine is taken the same way
//! ([`Turn::Linux`]) and runs on the [`HostLinux`] its embedder installed, what
//! it prints passed on to the device that asked as it prints it. A provider the
//! device's embedder registered ([`Workspaces::provide`](super::Workspaces::provide))
//! answers the requests that name it, once the workspace's grants admit the
//! asking device to the action. Every other app action reaches this device
//! through the session its members run, where the caller's certificate and the
//! workspace's policy
//! decide them ([`AppActionServer`](crate::app_serving::AppActionServer)).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationLog,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, Reservation,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_peer_state::StatePeer;
use tilo_peer_state::actions::{ActionService, ReadFuture, Workspaces};
use tilo_server_protocol::WorkspaceId;

use crate::work::{HostJobs, HostLinux, Job, LINUX_RESOURCE, Turn, WORK_PROVIDER};
use tokio::sync::Mutex as AsyncMutex;

use super::GovernedWorkspace;

/// The workspaces a host governs, by workspace: what [`Workspaces`](super::Workspaces)
/// keeps and this action service reads.
pub(crate) type WorkspacesMap = AsyncMutex<BTreeMap<String, Arc<AsyncMutex<GovernedWorkspace>>>>;

/// The work this device takes for the devices of its workspaces, shared by the
/// action service that answers them.
///
/// The action service is built before any embedder hands the workspaces a job
/// runner, so [`Self::serve`] fills it in; a device without one takes no turn.
pub(crate) struct DeviceWork {
    jobs: std::sync::RwLock<Option<Arc<dyn HostJobs>>>,
    /// The Linux machines this device runs, when an embedder installed them.
    linux: std::sync::RwLock<Option<Arc<dyn HostLinux>>>,
    /// The Linux runs taken here that have not ended: a cancel of one reaches
    /// the machine.
    linux_running: std::sync::Mutex<std::collections::HashSet<InvocationId>>,
    /// Moves on every change of a turn taken here, so a device following one
    /// looks again then.
    moved: tokio::sync::watch::Sender<u64>,
    /// The agent engine this device runs, once it runs one.
    agents: std::sync::RwLock<Option<Arc<dyn crate::work::AgentEngine>>>,
    /// The workspaces this device holds, as the action service decides them:
    /// who may ask is what each workspace's members say.
    workspaces: Arc<WorkspacesMap>,
    /// The workspaces a turn is claimed in, once they are open.
    turns: std::sync::OnceLock<super::WeakWorkspaces>,
    log: std::sync::Mutex<InvocationLog<InvocationState>>,
    /// The workspaces' governance, once they open: what a member device asks of it here.
    governance: std::sync::OnceLock<std::sync::Weak<super::actions::Shared>>,
    /// The providers the embedder registered, by the provider id a request names.
    provided: std::sync::RwLock<BTreeMap<&'static str, Provided>>,
    /// Which registered provider took each invocation, so its observations and its
    /// cancellation reach that provider.
    taken: std::sync::Mutex<HashMap<InvocationId, &'static str>>,
    runtime: tokio::runtime::Handle,
}

/// A provider registered on the device action service.
type Provided = Arc<dyn ActionProvider + Send + Sync>;

/// How many invocations a registered provider took are remembered for routing their
/// observations; the oldest are let go beyond it, and their provider is asked no more.
const TAKEN_KEPT: usize = 4096;

impl DeviceWork {
    /// Answer the workspaces' governance actions through `governance` from now on.
    pub(crate) fn govern_with(&self, governance: std::sync::Weak<super::actions::Shared>) {
        let _ = self.governance.set(governance);
    }

    /// The workspaces' governance, while it serves: held weakly, as it holds the workspaces.
    fn governance(&self) -> Option<super::ServiceWorkspaceActions> {
        let shared = self.governance.get()?.upgrade()?;
        Some(super::ServiceWorkspaceActions { shared })
    }

    /// `request`, an invocation of a workspace's governance by a device of its roster, decided
    /// as the workspace's governance decides it for that device's certificate.
    fn govern(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (Some(governance), Some(device), Some(workspace)) = (
            self.governance(),
            caller.device_public_key().copied(),
            caller.workspace().and_then(WorkspaceId::parse),
        ) else {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        };
        let workspaces = Arc::clone(&self.workspaces);
        let caller = caller.clone();
        let (completer, answer) = tilo_actions::remote::pending();
        self.runtime.spawn(async move {
            let held = workspaces.lock().await.get(&workspace.to_string()).cloned();
            let certificate = match held {
                Some(held) => held
                    .lock()
                    .await
                    .peer()
                    .certificates()
                    .into_iter()
                    .find(|certificate| certificate.device_public_key == device),
                None => None,
            };
            let Some(certificate) = certificate else {
                completer.complete(Err(ActionCallError::NotPermitted));
                return;
            };
            let receipt = governance
                .in_workspace_as(workspace, &certificate)
                .invoke(&caller, request)
                .await;
            completer.complete(receipt.unwrap_or(Err(ActionCallError::Unanswered)));
        });
        answer
    }

    /// Run the turns this device takes through `jobs`.
    pub(super) fn serve(&self, jobs: Arc<dyn HostJobs>) {
        *self.jobs.write_recover() = Some(jobs);
    }

    /// Run the Linux turns this device takes on `linux`.
    pub(super) fn serve_linux(&self, linux: Arc<dyn HostLinux>) {
        *self.linux.write_recover() = Some(linux);
    }

    /// The Linux machines, when an embedder installed them.
    pub(crate) fn linux(&self) -> Option<Arc<dyn HostLinux>> {
        self.linux.read_recover().clone()
    }

    /// Note `state` of a turn `caller` asked for, and wake whoever follows it.
    fn note(&self, caller: &Caller, state: InvocationState) {
        let invocation = state.invocation();
        if let Some(logged) = self.log.lock_recover().get_mut(caller, &invocation) {
            *logged = state;
        }
        self.moved
            .send_modify(|moves| *moves = moves.wrapping_add(1));
    }

    /// Claim turns in `workspaces` from now on: the workspaces this work belongs to,
    /// held without keeping them alive.
    pub(super) fn claim_in(&self, workspaces: super::WeakWorkspaces) {
        let _ = self.turns.set(workspaces);
    }

    /// The job runner, when an embedder installed one.
    pub(crate) fn jobs(&self) -> Option<Arc<dyn HostJobs>> {
        self.jobs.read_recover().clone()
    }

    /// The device action service's work for the workspaces in `workspaces`.
    ///
    /// # Panics
    ///
    /// Outside a runtime context, where there is nothing to answer on.
    fn new(workspaces: Arc<WorkspacesMap>) -> Arc<Self> {
        Arc::new(Self {
            jobs: std::sync::RwLock::default(),
            linux: std::sync::RwLock::default(),
            linux_running: std::sync::Mutex::default(),
            moved: tokio::sync::watch::Sender::new(0),
            agents: std::sync::RwLock::new(None),
            workspaces,
            turns: std::sync::OnceLock::new(),
            log: std::sync::Mutex::default(),
            governance: std::sync::OnceLock::new(),
            provided: std::sync::RwLock::default(),
            taken: std::sync::Mutex::default(),
            runtime: tokio::runtime::Handle::current(),
        })
    }

    /// Answer the requests that name `provider` with `answers` from now on.
    pub(crate) fn provide(&self, provider: &'static str, answers: Provided) {
        self.provided.write_recover().insert(provider, answers);
    }

    /// The registered provider `provider` names, when there is one.
    fn provided(&self, provider: &str) -> Option<(&'static str, Provided)> {
        self.provided
            .read_recover()
            .get_key_value(provider)
            .map(|(name, answers)| (*name, Arc::clone(answers)))
    }

    /// The registered provider that took `id`, when one did.
    fn taker(&self, id: &InvocationId) -> Option<Provided> {
        let name = *self.taken.lock_recover().get(id)?;
        self.provided(name).map(|(_, answers)| answers)
    }

    /// Hand `request` to the registered provider `answers` once the workspace the caller asks
    /// in admits its device to the action: the ordinary grant check, decided here once so no
    /// provider decides it again.
    fn admitted(
        self: &Arc<Self>,
        caller: &Caller,
        request: InvokeRequest,
        (name, answers): (&'static str, Provided),
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (Some(device), Some(workspace)) = (
            caller.device_public_key().copied(),
            caller.workspace().map(str::to_owned),
        ) else {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        };
        let work = Arc::clone(self);
        let caller = caller.clone();
        let (completer, answer) = tilo_actions::remote::pending();
        self.runtime.spawn(async move {
            let address = &request.target.address;
            if let Err(refused) = work
                .permits(device, &workspace, name, &address.resource, &address.action)
                .await
            {
                completer.complete(Err(refused));
                return;
            }
            {
                let mut taken = work.taken.lock_recover();
                if taken.len() >= TAKEN_KEPT {
                    taken.clear();
                }
                taken.insert(request.invocation, name);
            }
            let receipt = answers.invoke(&caller, request).await;
            completer.complete(receipt.unwrap_or(Err(ActionCallError::Unanswered)));
        });
        answer
    }

    /// Whether `workspace`'s grants let `device` run `action` on `resource` of `provider` now;
    /// the refusal its caller reads when they do not.
    async fn permits(
        &self,
        device: [u8; 32],
        workspace: &str,
        provider: &str,
        resource: &str,
        action: &str,
    ) -> Result<(), ActionCallError> {
        let kept = self.workspaces.lock().await.get(workspace).cloned();
        let Some(kept) = kept else {
            return Err(ActionCallError::Refused {
                reason: "this device does not keep that workspace".into(),
            });
        };
        let permitted = kept.lock().await.peer().permits_app_action(
            device,
            provider,
            resource,
            action,
            tilo_clock::wall(),
        );
        match permitted {
            Ok(true) => Ok(()),
            Ok(false) => Err(ActionCallError::Refused {
                reason: format!(
                    "your grants in this workspace do not let this device run {resource}#{action} \
                     of {provider}"
                ),
            }),
            Err(error) => Err(ActionCallError::Refused {
                reason: format!("this device is not admitted to the workspace: {error}"),
            }),
        }
    }

    /// Take `request`, a device's ask that this one take a turn of work
    /// (`tilo.work#take`): claim it when it is this device's turn and answer at
    /// once that it runs, then run it, renewing the claim while it does. A turn
    /// that is not this device's is refused with why, and nothing runs.
    fn take(
        self: &Arc<Self>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (Some(requester), Some(workspace)) = (
            caller.device_public_key().copied(),
            caller.workspace().map(str::to_owned),
        ) else {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        };
        let invocation = request.invocation;
        let turn = match turn_of(request, requester) {
            Ok(turn) => turn,
            Err(error) => return Pending::ready(Err(error)),
        };
        // A turn this device already took is answered with where it got to, and
        // is never run a second time.
        {
            let mut log = self.log.lock_recover();
            match log.reserve(caller, request) {
                Err(error) => return Pending::ready(Err(error)),
                Ok(Reservation::Admitted) => {
                    return Pending::ready(
                        log.get(caller, &invocation)
                            .map(InvocationState::receipt)
                            .ok_or(ActionCallError::UnknownInvocation),
                    );
                }
                Ok(Reservation::Reserved) => {}
            }
        }
        let caller = caller.clone();
        let work = Arc::clone(self);
        let (completer, answer) = tilo_actions::remote::pending();
        self.runtime.spawn(async move {
            let Some(workspaces) = work.turns.get().and_then(super::WeakWorkspaces::upgrade) else {
                work.log.lock_recover().abandon(&invocation);
                completer.complete(Err(not_served()));
                return;
            };
            let claim = match crate::work::takes_turn(&workspaces, &workspace, &turn).await {
                Ok(claim) => claim,
                Err(not_taken) => {
                    work.log.lock_recover().abandon(&invocation);
                    completer.complete(Err(ActionCallError::Refused {
                        reason: not_taken.to_string(),
                    }));
                    return;
                }
            };
            let job = match turn {
                Turn::Invocation { job, .. } => job,
                Turn::Linux { run, .. } => {
                    work.run_linux(claim, &caller, *run, completer).await;
                    return;
                }
                // The take action asks for no agent's turn: the agent engine takes those.
                Turn::Agent { .. } => {
                    work.log.lock_recover().abandon(&invocation);
                    completer.complete(Err(ActionCallError::UnknownAction));
                    return;
                }
            };
            let accepted =
                InvocationState::new(invocation, InvocationUpdate::Accepted { invocation });
            let receipt = accepted.receipt();
            work.log.lock_recover().complete(&invocation, accepted);
            completer.complete(Ok(receipt));
            let output = match work.jobs() {
                Some(jobs) => {
                    run_claimed(
                        jobs.as_ref(),
                        &claim,
                        &workspace,
                        &job,
                        requester,
                        invocation,
                    )
                    .await
                }
                None => Err(not_served()),
            };
            claim.release().await;
            let update = match output {
                Ok(output) => InvocationUpdate::Succeeded { invocation, output },
                Err(error) => InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: error.code().into(),
                        message: error.to_string(),
                        details: None,
                        retryable: false,
                    },
                },
            };
            work.note(&caller, InvocationState::new(invocation, update));
        });
        answer
    }

    /// Run `run`, the Linux turn `claim` is for, on this device's machines: the
    /// device that asked is answered with what admitting it there answered,
    /// and follows it to its end, each update passed on as the machine makes
    /// it; the claim is renewed while it runs, and a claim another device
    /// turned out to win stops the run there.
    async fn run_linux(
        self: &Arc<Self>,
        claim: crate::work::Claim,
        caller: &Caller,
        mut run: InvokeRequest,
        completer: tilo_actions::remote::Completer<Result<InvocationReceipt, ActionCallError>>,
    ) {
        let invocation = run.invocation;
        let Some(linux) = self.linux() else {
            claim.release().await;
            self.log.lock_recover().abandon(&invocation);
            completer.complete(Err(not_served()));
            return;
        };
        let runs = linux.runs();
        run.invocation = invocation;
        let admitted = runs
            .invoke(caller, run)
            .await
            .unwrap_or(Err(ActionCallError::Unanswered));
        let receipt = match admitted {
            Ok(receipt) if !receipt.status.is_terminal() => receipt,
            Ok(receipt) => {
                claim.release().await;
                let state =
                    InvocationState::new(invocation, settled_update(invocation, receipt.status));
                self.log.lock_recover().complete(&invocation, state.clone());
                completer.complete(Ok(state.receipt()));
                return;
            }
            Err(error) => {
                claim.release().await;
                self.log.lock_recover().abandon(&invocation);
                completer.complete(Err(error));
                return;
            }
        };
        self.log.lock_recover().complete(
            &invocation,
            InvocationState::new(invocation, InvocationUpdate::Accepted { invocation }),
        );
        self.linux_running.lock_recover().insert(invocation);
        completer.complete(Ok(receipt));
        let mut renewal = tokio::time::interval(RENEWAL);
        renewal.tick().await;
        let mut seen = None;
        loop {
            if let Some(state) = runs.observe(caller, &invocation)
                && seen.as_ref() != Some(&state.update)
            {
                seen = Some(state.update.clone());
                let ended = state.update.is_terminal();
                self.note(caller, state);
                if ended {
                    break;
                }
            }
            let change = runs.next_change(caller, &invocation);
            tokio::select! {
                () = async {
                    match change {
                        Some(change) => drop(change.await),
                        None => tokio::time::sleep(LINUX_LOOK).await,
                    }
                } => {}
                _ = renewal.tick() => {
                    if !claim.renew().await {
                        drop(runs.cancel(caller, &invocation));
                        self.note(caller, InvocationState::new(invocation, InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "unavailable".into(),
                                message: "another device took this turn".into(),
                                details: None,
                                retryable: true,
                            },
                        }));
                        break;
                    }
                }
            }
        }
        self.linux_running.lock_recover().remove(&invocation);
        claim.release().await;
    }
}

/// The update a run that ended as it was admitted ended with.
fn settled_update(
    invocation: InvocationId,
    status: tilo_actions::remote::ReceiptStatus,
) -> InvocationUpdate {
    use tilo_actions::remote::ReceiptStatus;
    match status {
        ReceiptStatus::Completed { output } => InvocationUpdate::Succeeded { invocation, output },
        ReceiptStatus::Failed { error } => InvocationUpdate::Failed { invocation, error },
        ReceiptStatus::Cancelled { message } => InvocationUpdate::Cancelled {
            invocation,
            message,
        },
        ReceiptStatus::Accepted | ReceiptStatus::Rejected { .. } => {
            InvocationUpdate::Accepted { invocation }
        }
    }
}

/// The turn `request`, asked by `requester`, asks this device to take.
fn turn_of(request: &InvokeRequest, requester: [u8; 32]) -> Result<Turn, ActionCallError> {
    tilo_actions::appearance::refuse_here(request)?;
    let tilo_actions::remote::InvokeArgument::Json(asked) = &request.argument else {
        return Err(ActionCallError::InvalidRequest {
            reason: "the work asked names nothing to run".into(),
        });
    };
    let asked = serde_json::Value::Object(asked.clone());
    let invalid = |error: serde_json::Error| ActionCallError::InvalidRequest {
        reason: format!("the work asked does not read: {error}"),
    };
    let id = request.invocation;
    if request.target.address.resource == LINUX_RESOURCE {
        return Ok(Turn::Linux {
            id,
            requester,
            run: Box::new(serde_json::from_value(asked).map_err(invalid)?),
        });
    }
    Ok(Turn::Invocation {
        id,
        requester,
        job: serde_json::from_value(asked).map_err(invalid)?,
    })
}

/// How long a Linux run is left before it is looked at again, when its
/// machine cannot say it changed.
const LINUX_LOOK: Duration = Duration::from_millis(100);

/// How often a running turn renews its claim: well inside the minute after
/// which a silent claim may be taken over.
const RENEWAL: Duration = Duration::from_secs(20);

/// Run `job` through `jobs` while `claim` holds, renewing it as it runs; a
/// claim another device turned out to win stops the run here.
async fn run_claimed(
    jobs: &dyn HostJobs,
    claim: &crate::work::Claim,
    workspace: &str,
    job: &Job,
    requester: [u8; 32],
    invocation: InvocationId,
) -> Result<ActionOutput, ActionCallError> {
    let run = jobs.run(workspace, job, requester, invocation);
    tokio::pin!(run);
    loop {
        tokio::select! {
            output = &mut run => {
                return output.unwrap_or(Err(ActionCallError::Unavailable {
                    reason: "this device stopped following the job".into(),
                }));
            }
            () = tokio::time::sleep(RENEWAL) => {
                if !claim.renew().await {
                    jobs.cancel(invocation);
                    return Err(ActionCallError::Unavailable {
                        reason: "another device took this turn".into(),
                    });
                }
            }
        }
    }
}

/// The device action service of the workspaces in `workspaces`, as the device
/// whose signing key is `device`.
///
/// # Panics
///
/// Outside a runtime context, where there is nothing to answer on.
pub(crate) fn action_service(
    workspaces: Arc<WorkspacesMap>,
    device: Arc<dyn collab_types::signer::DeviceSigner>,
) -> (ActionService, Arc<DeviceWork>) {
    let work = DeviceWork::new(Arc::clone(&workspaces));
    let action_service = ActionService::over(
        device,
        Arc::new(Answers(Arc::clone(&work))),
        Arc::new(HeldWorkspaces(workspaces)),
    );
    (action_service, work)
}

/// The workspaces this device holds, as the action service reads them: each is
/// locked for the read, so a change of its members reaches the action service
/// as it happens.
struct HeldWorkspaces(Arc<WorkspacesMap>);

impl Workspaces for HeldWorkspaces {
    fn read<'a>(
        &'a self,
        workspace: &'a str,
        read: &'a mut (dyn FnMut(&StatePeer) + Send),
    ) -> ReadFuture<'a> {
        Box::pin(async move {
            let held = self.0.lock().await.get(workspace).cloned();
            // A workspace this device does not hold is one it is no member of:
            // there is no state here to answer from.
            let Some(held) = held else {
                return false;
            };
            let held = held.lock().await;
            read(&held.peer());
            true
        })
    }
}

/// This device's answers on the device action service: the turns of work it
/// takes, and its workspaces' governance actions.
///
/// An app action reaches the device through the session its members run, which
/// carries the caller's certificate for the workspace's policy to decide; a
/// request for one that arrives here is refused as one this device does not
/// serve, so its caller learns at once instead of waiting.
pub(crate) struct Answers(pub(crate) Arc<DeviceWork>);

fn not_served() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "this device answers only turns of work and its workspaces' governance on the device action service".into(),
    }
}

impl ActionProvider for Answers {
    fn serves(&self, provider: &str) -> bool {
        provider == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
            || provider == WORK_PROVIDER
            || self.0.provided(provider).is_some()
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        match query
            .provider
            .as_deref()
            .and_then(|name| self.0.provided(name))
        {
            Some((_, answers)) => answers.query(caller, query),
            None => Pending::ready(Err(not_served())),
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if request.target.provider == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER {
            return self.0.govern(caller, request);
        }
        if request.target.provider == WORK_PROVIDER {
            return self.0.take(caller, &request);
        }
        if let Some(provided) = self.0.provided(&request.target.provider) {
            return self.0.admitted(caller, request, provided);
        }
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        if let Some(answers) = self.0.taker(id) {
            return answers.observe(caller, id);
        }
        let logged = self.0.log.lock_recover().get(caller, id).cloned();
        if logged.is_some() {
            return logged;
        }
        let workspace = caller.workspace().and_then(WorkspaceId::parse)?;
        self.0
            .governance()?
            .in_workspace(workspace)
            .observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        if let Some(answers) = self.0.taker(id) {
            return answers.next_change(caller, id);
        }
        self.0.log.lock_recover().get(caller, id)?;
        let mut moved = self.0.moved.subscribe();
        let (changed, change) = tilo_actions::remote::pending();
        self.0.runtime.spawn(async move {
            // Bounded, as the action service's contract asks: a change missed costs a look.
            let _ = tokio::time::timeout(Duration::from_secs(1), moved.changed()).await;
            changed.complete(());
        });
        Some(change)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        if let Some(answers) = self.0.taker(id) {
            return answers.cancel(caller, id);
        }
        if self.0.linux_running.lock_recover().contains(id) {
            return match self.0.linux() {
                Some(linux) => linux.runs().cancel(caller, id),
                None => Pending::ready(Err(not_served())),
            };
        }
        let Some(jobs) = self.0.jobs() else {
            return Pending::ready(Err(not_served()));
        };
        match self.0.log.lock_recover().get(caller, id) {
            Some(state) if state.is_terminal() => {
                Pending::ready(Err(ActionCallError::NotCancellable {
                    reason: "the job already finished here".into(),
                }))
            }
            Some(_) => {
                jobs.cancel(*id);
                Pending::ready(Ok(()))
            }
            None => Pending::ready(Err(ActionCallError::UnknownInvocation)),
        }
    }
}

/// What a turn of work reads of the workspaces it is claimed in ([`crate::work`]).
impl super::Workspaces {
    /// The job runner an embedder installed ([`Self::run_jobs`]).
    pub(crate) fn host_jobs(&self) -> Option<Arc<dyn HostJobs>> {
        self.inner.jobs.jobs()
    }

    /// The Linux machines an embedder installed ([`Self::run_linux`]).
    pub(crate) fn host_linux(&self) -> Option<Arc<dyn HostLinux>> {
        self.inner.jobs.linux()
    }

    /// Whether `device` may run `job`'s action in `workspace` now, as the
    /// workspace's per-action permission says, whatever kind of member it is.
    pub(crate) async fn permits_job(&self, device: [u8; 32], workspace: &str, job: &Job) -> bool {
        self.reads_peer(workspace, |peer| {
            peer.permits_app_action(
                device,
                &job.app,
                &job.resource,
                &job.action,
                tilo_clock::wall(),
            )
        })
        .await
    }

    /// Whether `device` may open `app`'s record `resource` in `workspace` now.
    pub(crate) async fn opens_record(
        &self,
        device: [u8; 32],
        workspace: &str,
        app: &str,
        resource: &str,
    ) -> bool {
        self.reads_peer(workspace, |peer| {
            peer.opens_app_record(device, app, resource, tilo_clock::wall())
        })
        .await
    }

    /// The agent engine this device runs, which takes the turns of agents'
    /// turns ([`crate::work::Turn::Agent`]); `None` where it runs none.
    pub(crate) fn agent_keys(&self) -> Option<Arc<dyn crate::work::AgentEngine>> {
        self.inner.jobs.agents.read_recover().clone()
    }

    /// Resolve the live signing scope of the agent which signs a turn for this keeper.
    pub async fn agent_access(
        &self,
        workspace: &str,
        signer: &str,
        keeper: &[u8; 32],
    ) -> Result<crate::work::AgentAccess, crate::work::Ineligible> {
        let access = self
            .agent_keys()
            .ok_or(crate::work::Ineligible::RunsNoAgents)?
            .access(workspace, signer, keeper)
            .await?;
        if matches!(access, crate::work::AgentAccess::Device) {
            crate::work::device_of(self, keeper).await?;
        }
        Ok(access)
    }

    /// Whether the caller still holds the same scope that authorizes this app now.
    pub async fn agent_may_act(
        &self,
        workspace: &str,
        caller: &tilo_actions::remote::Caller,
        app: &str,
    ) -> bool {
        let (Some(agent), Some(keeper)) = (caller.approved_agent(), caller.user_public_key())
        else {
            return false;
        };
        caller.workspace() == Some(workspace)
            && self
                .agent_access(workspace, agent, keeper)
                .await
                .is_ok_and(|access| access.matches_caller(caller) && access.permits_app(app))
    }

    /// This device runs `engine` from now on.
    #[cfg(any(test, all(feature = "agents", not(target_arch = "wasm32"))))]
    pub(crate) fn run_agents(&self, engine: Arc<dyn crate::work::AgentEngine>) {
        *self.inner.jobs.agents.write_recover() = Some(engine);
    }

    /// What `read` answers of `workspace` as this device holds it; `false`
    /// where it holds none, or the device is no member.
    async fn reads_peer<E>(
        &self,
        workspace: &str,
        read: impl FnOnce(&tilo_peer_state::StatePeer) -> Result<bool, E>,
    ) -> bool {
        let held = self.inner.workspaces.lock().await.get(workspace).cloned();
        let Some(held) = held else {
            return false;
        };
        let held = held.lock().await;
        read(&held.peer()).unwrap_or(false)
    }

    /// The device whose claims [`Self::write_lease`] writes in `workspace`: the
    /// device its member copy acts as where another device serves it, else this
    /// one.
    pub(crate) async fn lease_device(&self, workspace: &str) -> [u8; 32] {
        if self.is_member(workspace).await.unwrap_or(false)
            && let Ok(member) = self.member_identity()
        {
            return member.cert.device_public_key;
        }
        self.serving_certificate().device_public_key
    }

    /// Claim the lease `id` of `workspace` as this device for `ttl_ms`, or let go
    /// of it with `0`: a change of this device's own, written where this device
    /// keeps the workspace and carried to its other holders.
    pub(crate) async fn write_lease(
        &self,
        workspace: &str,
        id: &str,
        ttl_ms: u64,
    ) -> Result<(), String> {
        let command = tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Lease {
            id: id.to_owned(),
            ttl_ms,
            rank: tilo_peer_state::unranked(),
        };
        if self.is_member(workspace).await? {
            let commit = self.member_membership_write(workspace, command).await?;
            self.commit_member_document(workspace, commit).await?;
        } else {
            self.commit_own_membership(workspace, command, None)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}
