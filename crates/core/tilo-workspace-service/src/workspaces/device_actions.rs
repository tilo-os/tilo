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
//! actions (inviting someone, listing and cancelling invitations) and the
//! models its work runs on ([`tilo_actions::remote::person_models`]) are
//! decided as the workspace's governance decides them for the asking device's
//! certificate, and so are the host's own operations (`account/…`: the person's
//! workspace list, making and deleting workspaces) for a device that asks them
//! as itself. A run on a workspace's Linux machine is taken the same way
//! ([`Turn::Linux`]) and runs on the [`HostLinux`] its embedder installed, what
//! it prints passed on to the device that asked as it prints it. A provider the
//! device's embedder registered ([`Workspaces::provide`](super::Workspaces::provide))
//! answers the requests that name it, once the workspace's grants admit the
//! asking device to the action. Every other app action reaches this device
//! through the session its members run, where the caller's certificate and the
//! workspace's policy
//! decide them ([`AppActionServer`](crate::app_serving::AppActionServer)).

use std::collections::BTreeMap;
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
use tilo_server_protocol::workspaces::{
    HOST_ACTIONS_PROVIDER, LIST_PERSONAL, PERSONAL_AFTER, PersonalList,
};
use tokio::sync::Mutex as AsyncMutex;

use super::GovernedWorkspace;

#[cfg(test)]
mod acting_tests;

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
    /// The providers registered on this device: what another device's call naming one reaches.
    pub(super) providers: super::providers::Providers,
    /// Whether agents from the person's other devices are let in, when a test says so
    /// itself rather than reading the device's settings.
    #[cfg(any(test, feature = "test-support"))]
    agents_from_other_devices: std::sync::RwLock<Option<bool>>,
    runtime: tokio::runtime::Handle,
}

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

    /// `request`, an invocation of a workspace's governance or of its models by a device of its
    /// roster, decided as the workspace's governance decides it for that device's certificate.
    fn govern(
        self: &Arc<Self>,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.as_certified(
            caller,
            move |_, governance, workspace, certificate, caller| {
                Box::pin(async move {
                    governance
                        .in_workspace_as(workspace, &certificate)
                        .invoke(&caller, request)
                        .await
                        .unwrap_or(Err(ActionCallError::Unanswered))
                })
            },
        )
    }

    /// `request`, one of the host's own operations (`account/…`) asked by a device of a
    /// workspace's roster as itself, decided as the host decides it for that device's
    /// certificate. A read of the person's workspace list that names the version the device
    /// holds ([`PERSONAL_AFTER`]) waits until the list moves past it ([`Self::follow_list`]).
    fn account(
        self: &Arc<Self>,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.as_certified(caller, move |work, governance, _, certificate, caller| {
            Box::pin(async move {
                let host = governance.as_host(&certificate);
                match followed_version(&request) {
                    Some(after) => {
                        work.follow_list(&host, &certificate, &caller, request, after)
                            .await
                    }
                    None => host
                        .invoke(&caller, request)
                        .await
                        .unwrap_or(Err(ActionCallError::Unanswered)),
                }
            })
        })
    }

    /// What `decide` answers for `caller`, a device asking in a workspace whose roster holds its
    /// certificate, acting as the workspace says that device acts, handed this work, the
    /// workspaces' governance, the workspace, that certificate and the caller; a device off the
    /// roster, or a caller acting as anyone else on it, is not permitted, and with no governance
    /// here nothing is known.
    fn as_certified(
        self: &Arc<Self>,
        caller: &Caller,
        decide: impl FnOnce(
            Arc<Self>,
            super::ServiceWorkspaceActions,
            WorkspaceId,
            tilo_user_identity::DeviceCert,
            Caller,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<InvocationReceipt, ActionCallError>> + Send,
            >,
        > + Send
        + 'static,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let work = Arc::clone(self);
        let (Some(governance), Some(device), Some(workspace)) = (
            self.governance(),
            caller.device_public_key().copied(),
            caller.workspace().and_then(WorkspaceId::parse),
        ) else {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        };
        let caller = caller.clone();
        let (completer, answer) = tilo_actions::remote::pending();
        self.runtime.spawn(async move {
            let held = work
                .workspaces
                .lock()
                .await
                .get(&workspace.to_string())
                .cloned();
            // The device's certificate, and who the workspace says acts on it.
            let listed = match held {
                Some(held) => {
                    let held = held.lock().await;
                    let peer = held.peer();
                    let listed = peer
                        .certificates()
                        .into_iter()
                        .find(|certificate| certificate.device_public_key == device)
                        .map(|certificate| {
                            let by = peer
                                .permissions(device, tilo_clock::wall())
                                .map(|access| access.acting.by);
                            (certificate, by.ok())
                        });
                    drop(peer);
                    drop(held);
                    listed
                }
                None => None,
            };
            // What the host answers here it answers for whoever the device's certificate acts
            // as: a caller acting as anyone else on it (a bot, an app, an agent the device
            // handed its requests to) is not answered as that device's own.
            let acting = caller.acting();
            let certificate = match listed {
                Some((certificate, Some(by)))
                    if !acting.handed && acting.app().is_none() && acting.by == by =>
                {
                    certificate
                }
                _ => {
                    completer.complete(Err(ActionCallError::NotPermitted));
                    return;
                }
            };
            completer.complete(decide(work, governance, workspace, certificate, caller).await);
        });
        answer
    }

    /// Answer `request`, a read of `certificate`'s person's workspace list naming `after`, the
    /// version their device holds: at once when the list is another, else once it moves or
    /// [`LIST_FOLLOWED`] went by, whichever comes first. Each read is decided by `host` as any
    /// other; the invocation is followed here, as a turn is.
    async fn follow_list<H: ActionProvider + Clone + Send + Sync + 'static>(
        self: &Arc<Self>,
        host: &H,
        _certificate: &tilo_user_identity::DeviceCert,
        caller: &Caller,
        request: InvokeRequest,
        after: String,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let invocation = request.invocation;
        {
            let mut log = self.log.lock_recover();
            if log.reserve(caller, &request)? == Reservation::Admitted {
                return log
                    .get(caller, &invocation)
                    .map(InvocationState::receipt)
                    .ok_or(ActionCallError::UnknownInvocation);
            }
        }
        let workspaces = self
            .turns
            .get()
            .and_then(super::WeakWorkspaces::upgrade)
            .ok_or_else(not_served)?;
        // Followed before the first read, so a change between the read and the
        // wait is not missed.
        let mut changed = workspaces.personal_changes();
        let first = match list_now(host, caller, &request).await {
            Ok(list) => list,
            Err(error) => {
                self.log.lock_recover().abandon(&invocation);
                return Err(error);
            }
        };
        if PersonalList::version(&first) != after {
            let state = listed(invocation, first);
            let receipt = state.receipt();
            self.log.lock_recover().complete(&invocation, state);
            return Ok(receipt);
        }
        let accepted = InvocationState::new(invocation, InvocationUpdate::Accepted { invocation });
        let receipt = accepted.receipt();
        self.log.lock_recover().complete(&invocation, accepted);
        let (work, caller, host) = (Arc::downgrade(self), caller.clone(), host.clone());
        self.runtime.spawn(async move {
            let _ = tokio::time::timeout(LIST_FOLLOWED, changed.changed()).await;
            let update = match list_now(&host, &caller, &request).await {
                Ok(list) => listed(invocation, list).update,
                Err(error) => InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: error.code().into(),
                        message: error.to_string(),
                        details: None,
                        retryable: true,
                    },
                },
            };
            if let Some(work) = work.upgrade() {
                work.note(&caller, InvocationState::new(invocation, update));
            }
        });
        Ok(receipt)
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

    /// Whether this device lets agents from the person's other devices work here: what
    /// the device's settings say now, or what a test said.
    #[cfg_attr(
        any(test, feature = "test-support"),
        expect(
            clippy::significant_drop_in_scrutinee,
            reason = "the guard is released when the condition ends, and nothing waits while it is held"
        )
    )]
    fn accepts_agents_from_other_devices(&self) -> bool {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(accepts) = *self.agents_from_other_devices.read_recover() {
            return accepts;
        }
        settings_store::mcp_server::accepts_own_devices(&settings_store::load())
    }

    /// Let agents from the person's other devices work here, or not, as a test says.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn accept_agents_from_other_devices(&self, accepts: bool) {
        *self.agents_from_other_devices.write_recover() = Some(accepts);
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
            providers: super::providers::Providers::default(),
            #[cfg(any(test, feature = "test-support"))]
            agents_from_other_devices: std::sync::RwLock::default(),
            runtime: tokio::runtime::Handle::current(),
        })
    }

    /// Answer `request`, what an agent's turn asks of what its workspace keeps, for `caller`,
    /// as the workspaces this work belongs to decide it ([`super::Workspaces::answer_turn`]).
    #[cfg(not(target_arch = "wasm32"))]
    fn answer_turn(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let Some(workspaces) = self.turns.get().and_then(super::WeakWorkspaces::upgrade) else {
            return Pending::ready(Err(not_served()));
        };
        let caller = caller.clone();
        let (completer, answer) = tilo_actions::remote::pending();
        self.runtime.spawn(async move {
            completer.complete(workspaces.answer_turn(&caller, &request).await);
        });
        answer
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
                // The take action asks for no agent's turn, no automation's firing
                // and no action that runs once: the agent engine, the trigger
                // runner and the app's own process take those.
                Turn::Agent { .. } | Turn::Firing { .. } | Turn::Once { .. } => {
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

/// What an agent another of the person's devices handed here reads while this device takes none.
const DEVICE_AGENTS_NOT_ACCEPTED: &str = "device-agents-not-accepted";

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
) -> (ActionService, Arc<DeviceWork>, Arc<Answers>) {
    let work = DeviceWork::new(Arc::clone(&workspaces));
    let answers = Arc::new(Answers {
        work: Arc::clone(&work),
        apps: std::sync::OnceLock::new(),
    });
    let action_service = ActionService::over(
        device,
        Arc::clone(&answers) as Arc<dyn ActionProvider + Send + Sync>,
        Arc::new(HeldWorkspaces(workspaces)),
    );
    (action_service, work, answers)
}

/// The certificate this host holds for `device` in `workspace`, when it holds
/// one: what an app action server holds a caller to beyond who it is.
pub(crate) async fn certificate_of(
    workspaces: &WorkspacesMap,
    workspace: &str,
    device: [u8; 32],
) -> Option<tilo_user_identity::DeviceCert> {
    let kept = workspaces.lock().await.get(workspace).cloned()?;
    let kept = kept.lock().await;
    kept.peer()
        .certificates()
        .into_iter()
        .find(|certificate| certificate.device_public_key == device)
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
/// takes, its workspaces' governance actions, and the apps this host serves.
///
/// The apps this host serves join once its embedder hands them over
/// ([`Self::serve_apps`]): a request for one of them is answered by the app
/// action server, and a page action only a window on some device runs is
/// handed to that device. Until then, a request for one is refused as one this
/// device does not serve, so its caller learns at once instead of waiting.
pub(crate) struct Answers {
    pub(crate) work: Arc<DeviceWork>,
    /// The apps this host serves, once its embedder installed them.
    pub(crate) apps: std::sync::OnceLock<crate::app_serving::Served>,
}

impl Answers {
    /// Answer `workspace`s' apps through `served` from now on.
    pub(crate) fn serve_apps(&self, served: crate::app_serving::Served) {
        let _ = self.apps.set(served);
    }

    /// The apps this host serves, when it serves any.
    fn apps(&self) -> Option<&crate::app_serving::Served> {
        self.apps.get()
    }
}

/// How long a read of the person's workspace list that names the version the asking device
/// holds waits for the list to move before it answers the same list again: inside the wait a
/// device following an invocation gives it.
const LIST_FOLLOWED: Duration = Duration::from_secs(25);

/// The version of the person's list `request` names as the one its device holds, when it is a
/// read of that list that follows it.
fn followed_version(request: &InvokeRequest) -> Option<String> {
    if request.target.address.action != LIST_PERSONAL {
        return None;
    }
    let tilo_actions::remote::InvokeArgument::Json(arguments) = &request.argument else {
        return None;
    };
    arguments
        .get(PERSONAL_AFTER)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// The person's list as `host` answers `request` now, read under an invocation of its own and
/// without the version it follows.
async fn list_now(
    host: &(impl ActionProvider + Sync),
    caller: &Caller,
    request: &InvokeRequest,
) -> Result<serde_json::Value, ActionCallError> {
    let mut read = request.clone();
    read.invocation = InvocationId::new();
    if let tilo_actions::remote::InvokeArgument::Json(arguments) = &mut read.argument {
        arguments.remove(PERSONAL_AFTER);
    }
    let receipt = host
        .invoke(caller, read)
        .await
        .unwrap_or(Err(ActionCallError::Unanswered))?;
    match receipt.status {
        tilo_actions::remote::ReceiptStatus::Completed { output } => {
            Ok(output.value.unwrap_or_default())
        }
        tilo_actions::remote::ReceiptStatus::Rejected { error } => Err(error),
        tilo_actions::remote::ReceiptStatus::Failed { error } => Err(ActionCallError::Refused {
            reason: error.message,
        }),
        tilo_actions::remote::ReceiptStatus::Accepted
        | tilo_actions::remote::ReceiptStatus::Cancelled { .. } => Err(ActionCallError::Unanswered),
    }
}

/// `invocation` answered with `list`.
fn listed(invocation: InvocationId, list: serde_json::Value) -> InvocationState {
    InvocationState::new(
        invocation,
        InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput {
                value: Some(list),
                ..ActionOutput::default()
            },
        },
    )
}

fn not_served() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "this device answers only turns of work and its workspaces' governance on the device action service".into(),
    }
}

/// Whether `provider` is answered by a workspace's governance here: its own actions, and the
/// models its work runs on, which the device holding the workspace keeps for its members' devices
/// as a host keeps them for a page.
fn governed(provider: &str) -> bool {
    provider == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
        || provider == tilo_actions::remote::person_models::PROVIDER
}

impl ActionProvider for Answers {
    /// Records are read by the device's one reader ([`super::RecordReads`]): the workspaces it
    /// holds for another device's certificate there, every other caller by the apps it serves.
    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        Some(Arc::new(super::RecordReads::of_device(
            self.work.turns.get().cloned(),
            self.apps().and_then(ActionProvider::resource_reads),
        )))
    }

    fn serves(&self, provider: &str) -> bool {
        governed(provider)
            || provider == HOST_ACTIONS_PROVIDER
            || provider == WORK_PROVIDER
            // The calls that wait for a person's yes, listed and answered from their records.
            || provider == tilo_actions::remote::asked::PROVIDER
            || provider == tilo_actions::remote::workspace_invocations::PROVIDER
            || self.work.providers.serves(provider)
            || self.apps().is_some()
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        if query
            .provider
            .as_deref()
            .is_some_and(|name| self.work.providers.serves(name))
        {
            return self
                .work
                .providers
                .query(caller, query)
                .expect("a provider that serves the call answers its query");
        }
        if let Some(served) = self.apps() {
            let answered = served.query(caller, query.clone());
            // A listing naming no provider names what runs in the workspace beside what the apps
            // do: the publishers this device's registered providers list, where they answer at
            // once, join the answer. A provider asked here for its own rows answers those alone.
            let publishers = if query.provider.is_none() {
                self.work.providers.publishers(caller, &query)
            } else {
                Vec::new()
            };
            if publishers.is_empty() {
                return answered;
            }
            let (completer, merged) = tilo_actions::remote::pending();
            self.work.runtime.spawn(async move {
                let result = match answered.await {
                    Some(Ok(mut page)) => {
                        for listed in publishers {
                            if !page.providers.iter().any(|listed_in| {
                                listed_in.provider == listed.provider
                                    && listed_in.workspace == listed.workspace
                            }) {
                                page.providers.push(listed);
                            }
                        }
                        Ok(page)
                    }
                    Some(Err(error)) => Err(error),
                    None => Err(ActionCallError::Unavailable {
                        reason: "the listing was not answered".to_owned(),
                    }),
                };
                completer.complete(result);
            });
            merged
        } else {
            Pending::ready(Err(not_served()))
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // An agent another of the person's devices handed its session to works here only while
        // this device's settings let such agents in.
        if caller.acting().handed && !self.work.accepts_agents_from_other_devices() {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: crate::i18n::message(
                    None,
                    DEVICE_AGENTS_NOT_ACCEPTED,
                    &tilo_history::FluentArgs::new(),
                ),
            }));
        }
        // What an agent's turn asks of what its workspace keeps for the person who asked it
        // (their vault's uses by reference, mail, calendars, connections) is decided for that
        // turn, before anything a person's device asks of the same providers.
        #[cfg(not(target_arch = "wasm32"))]
        if super::hosted_use::answers(caller, &request.target) {
            return self.work.answer_turn(caller, request);
        }
        if governed(&request.target.provider)
            || request.target.provider == tilo_actions::remote::asked::PROVIDER
            || tilo_actions::remote::asked::is_resolve(&request.target)
        {
            return self.work.govern(caller, request);
        }
        if request.target.provider == HOST_ACTIONS_PROVIDER {
            return self.work.account(caller, request);
        }
        if request.target.provider == WORK_PROVIDER {
            return self.work.take(caller, &request);
        }
        if self.work.providers.serves(&request.target.provider) {
            return self
                .work
                .providers
                .from_device(caller, request, &self.work.workspaces, &self.work.runtime)
                .expect("a provider that serves the call takes it");
        }
        // An address a provider registered on this device publishes is that provider's, whichever
        // provider token the call names: a workspace's own `app.<app>#open` is the open of an app
        // this device places, and page actions of what it placed are its own.
        if self
            .work
            .providers
            .published(&request.target, super::providers::Reach::Devices)
            .is_some()
        {
            return self
                .work
                .providers
                .from_device(caller, request, &self.work.workspaces, &self.work.runtime)
                .expect("a provider that publishes the call takes it");
        }
        if let Some(served) = self.apps() {
            served.invoke(caller, request)
        } else {
            Pending::ready(Err(ActionCallError::UnknownAction))
        }
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        if let Some(answers) = self.work.providers.observe(caller, id) {
            return answers;
        }
        let logged = self.work.log.lock_recover().get(caller, id).cloned();
        if logged.is_some() {
            return logged;
        }
        let workspace = caller.workspace().and_then(WorkspaceId::parse)?;
        if let Some(answers) = self
            .work
            .governance()
            .and_then(|governance| governance.in_workspace(workspace).observe(caller, id))
        {
            return Some(answers);
        }
        self.apps()?.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        if let Some(change) = self.work.providers.next_change(caller, id) {
            return change;
        }
        if self.work.log.lock_recover().get(caller, id).is_some() {
            let mut moved = self.work.moved.subscribe();
            let (changed, change) = tilo_actions::remote::pending();
            self.work.runtime.spawn(async move {
                // Bounded, as the action service's contract asks: a change missed costs a look.
                let _ = tokio::time::timeout(Duration::from_secs(1), moved.changed()).await;
                changed.complete(());
            });
            return Some(change);
        }
        self.apps()?.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        if let Some(cancelled) = self.work.providers.cancel(caller, id) {
            return cancelled;
        }
        if self.work.linux_running.lock_recover().contains(id) {
            return match self.work.linux() {
                Some(linux) => linux.runs().cancel(caller, id),
                None => Pending::ready(Err(not_served())),
            };
        }
        let Some(jobs) = self.work.jobs() else {
            return if let Some(served) = self.apps() {
                served.cancel(caller, id)
            } else {
                Pending::ready(Err(not_served()))
            };
        };
        match self.work.log.lock_recover().get(caller, id) {
            Some(state) if state.is_terminal() => {
                Pending::ready(Err(ActionCallError::NotCancellable {
                    reason: "the job already finished here".into(),
                }))
            }
            Some(_) => {
                jobs.cancel(*id);
                Pending::ready(Ok(()))
            }
            None => {
                if let Some(served) = self.apps() {
                    served.cancel(caller, id)
                } else {
                    Pending::ready(Err(ActionCallError::UnknownInvocation))
                }
            }
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
        self.permits_action(device, workspace, (&job.app, &job.resource, &job.action))
            .await
    }

    /// Whether `device` may run `action` (app, resource, action) in `workspace` now.
    pub(crate) async fn permits_action(
        &self,
        device: [u8; 32],
        workspace: &str,
        (app, resource, action): (&str, &str, &str),
    ) -> bool {
        self.reads_peer(workspace, |peer| {
            peer.permits_app_action(device, app, resource, action, tilo_clock::wall())
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

    /// A key of this device's own for `statement`: its device key signs a digest of the
    /// statement, and the key is made from that signature, so the device makes the same key every
    /// time and nothing secret is written down. The key never leaves the device; what it offers
    /// of it is its public key, in the ticket that asks for it to be approved.
    pub(crate) fn derived_device_key(&self, statement: &[u8]) -> ed25519_dalek::SigningKey {
        let digest = blake3::hash(statement);
        let signature = ed25519_dalek::Signer::sign(&self.inner.host.key, digest.as_bytes());
        ed25519_dalek::SigningKey::from_bytes(blake3::hash(&signature.to_bytes()).as_bytes())
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
        self.write_own_row(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Lease {
                id: id.to_owned(),
                ttl_ms,
                rank: tilo_peer_state::unranked(),
            },
        )
        .await
    }

    /// Keep that this device answered the schedule at `trigger` of the
    /// automation `automation`, marked `mark`, through `answered_ms`: the
    /// progress whichever device fires it next carries on from. The trigger
    /// runner is an apps build's with agents, and it is the runner's to ask.
    #[cfg(all(feature = "apps", feature = "agents"))]
    pub(crate) async fn answer_schedule(
        &self,
        workspace: &str,
        automation: &str,
        trigger: usize,
        mark: &str,
        answered_ms: i64,
    ) -> Result<(), String> {
        self.write_own_row(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::AnswerSchedule {
                automation: automation.to_owned(),
                trigger: u32::try_from(trigger).map_err(|error| error.to_string())?,
                mark: mark.to_owned(),
                answered_ms,
            },
        )
        .await
    }

    /// Keep `progress` as what this device made of `trigger` of the automation
    /// `automation`, marked `mark`, at its last look: what whichever device
    /// looks next carries on from.
    #[cfg(feature = "apps")]
    pub(crate) async fn keep_trigger_progress(
        &self,
        workspace: &str,
        automation: &str,
        trigger: usize,
        mark: &str,
        progress: serde_json::Value,
    ) -> Result<(), String> {
        self.write_own_row(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::KeepTriggerProgress {
                automation: automation.to_owned(),
                trigger: u32::try_from(trigger).map_err(|error| error.to_string())?,
                mark: mark.to_owned(),
                kept_ms: tilo_clock::unix_now_ms_i64(),
                progress,
            },
        )
        .await
    }

    /// Keep that this device did the work of the lease `id`, and what it
    /// answered: no device of the workspace takes it again.
    pub(crate) async fn write_done(
        &self,
        workspace: &str,
        id: &str,
        answer: serde_json::Value,
    ) -> Result<(), String> {
        self.write_own_row(
            workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::KeepDone {
                id: id.to_owned(),
                answer,
            },
        )
        .await
    }

    /// Write `command`, a row of this device's own in the workspace's state:
    /// a member copy's write sent to the device serving it, or a change of
    /// the workspace this device governs.
    async fn write_own_row(
        &self,
        workspace: &str,
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<(), String> {
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
