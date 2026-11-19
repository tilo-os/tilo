//! The device action service: what another device of a workspace asks this host
//! directly, with no member session in between.
//!
//! A host is a member device of the workspaces it governs: its own device
//! holds their governance and their state. So those workspaces answer the
//! devices' action service (`tilo-actions/1`) as any other member device does. A device
//! of the workspace asks whether this host would run a piece of work, and the
//! host answers with what it knows of itself ([`Offering`]) — the same round a
//! person's devices run among themselves, in which the host is one of the
//! devices asked, not the one that decides. Nothing here is privileged: the
//! question is checked against the workspace's roster as it stands, the answer
//! is the host's own state, and a workspace this host does not govern is not
//! held at all.
//!
//! An action whose declaration says it is a transform ([`HostJobs`]) is work
//! this host can take: it says so, and it runs the invocation that comes back
//! on the same action service as a compute job over content it keeps. A
//! workspace's governance actions (inviting someone, listing and cancelling
//! invitations) are answered here too, as the workspace's governance decides
//! them for the asking device's certificate in its roster. Every other app
//! action is not answered here: those reach the host through the session its
//! members run, where the caller's certificate and the workspace's policy
//! decide them ([`AppActionServer`](crate::app_serving::AppActionServer)).

use std::collections::BTreeMap;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use neighbour_wire::offer::{Answer, Decline, Power, Query, Route, Work};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationLog,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, Reservation,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_peer_state::StatePeer;
use tilo_peer_state::actions::{ActionService, Offering, ReadFuture, Workspaces};
use tilo_server_protocol::{Machine, Whose, WorkSetting, WorkspaceId};
use tokio::sync::Mutex as AsyncMutex;

use super::GovernedWorkspace;

/// The workspaces a host governs, by workspace: what [`Workspaces`](super::Workspaces)
/// keeps and this action service reads.
pub(crate) type WorkspacesMap = AsyncMutex<BTreeMap<String, Arc<AsyncMutex<GovernedWorkspace>>>>;

/// The work a host runs for the devices of its workspaces: an action that
/// declares a transform, run as a compute job over content the host keeps.
///
/// What it answers through is its own: the inputs it already holds, and how
/// busy it is. Two hosts of the same person differ only in that.
pub trait HostJobs: Send + Sync {
    /// Whether this host would run `work`'s transform in `workspace` now: the
    /// work declares one and this host has the package that performs it.
    fn runs(&self, workspace: &str, work: &Work) -> bool;

    /// Which of `work`'s inputs this host keeps already, so moving them is no
    /// part of what a run here costs.
    fn holds(&self, workspace: &str, work: &Work) -> Vec<[u8; 32]>;

    /// How many more jobs this host can start right now, when it keeps
    /// count: zero says it is at its limit and takes nothing more. `None`
    /// for a runner that takes what it is handed.
    fn free_slots(&self) -> Option<u32> {
        None
    }

    /// Run `request`, an invocation of work this host answered an offer for,
    /// as a compute job of `workspace` asked by the device `requester`, and
    /// answer with what it produced: each output kept where the workspace's
    /// devices fetch its content, addressed as `tilo.content:<hex>`.
    fn run(
        &self,
        workspace: &str,
        work: &Work,
        requester: [u8; 32],
        request: InvokeRequest,
    ) -> Pending<Result<ActionOutput, ActionCallError>>;

    /// Stop the job running here for `invocation`, when it still runs: what
    /// the device that asked for it gets when it stops following it.
    fn cancel(&self, invocation: InvocationId) {
        let _ = invocation;
    }
}

/// One offer this host answered: the device that asked, the invocation it
/// named, and the work it was answered for. An invocation runs only the
/// offer it names, made by the device that made it.
struct Offered {
    from: [u8; 32],
    invocation: InvocationId,
    workspace: String,
    app: String,
    resource: String,
    action: String,
    work: Work,
}

/// What a host offers the workspaces it holds, as the entry its person keeps
/// of it in their admin workspace says ([`Workspaces::set_offer`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offer {
    /// Gigabytes of storage it offers its workspaces.
    pub storage_gb: u64,
    /// Whether it runs its workspaces' actions and jobs.
    pub compute: bool,
    /// How many Linux machines it runs for its workspaces at once.
    pub machines: u32,
}

impl Offer {
    /// What a device takes before any entry says anything about it: it runs
    /// no work for anyone.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            storage_gb: 0,
            compute: false,
            machines: 0,
        }
    }

    /// What an entry naming `storage_gb`, `compute` and `machines` offers.
    #[must_use]
    pub const fn of(storage_gb: u64, compute: bool, machines: u32) -> Self {
        Self {
            storage_gb,
            compute,
            machines,
        }
    }
}

impl Default for Offer {
    /// What a host answers with before any entry is applied to it: its work
    /// setting alone decides ([`HostOffering`]).
    fn default() -> Self {
        Self {
            storage_gb: 0,
            compute: true,
            machines: u32::MAX,
        }
    }
}

/// How many offers this host remembers at once. A device asks and then
/// invokes at once; what it is not invoked on within this many later offers
/// is forgotten.
const MAX_OFFERED: usize = 256;

/// The jobs a host runs, shared by the offering that answers offers and the
/// dispatcher that answers invocations, and the work offered here.
///
/// The action service is built before any embedder hands the host a job runner, so
/// [`Self::serve`] fills it in. A host without one says it runs no app
/// actions, as it always did.
pub(crate) struct DeviceWork {
    jobs: std::sync::RwLock<Option<Arc<dyn HostJobs>>>,
    /// What this host offers, as its person's entry for it says
    /// ([`Workspaces::set_offer`]); it takes no work until one is set.
    offer: std::sync::RwLock<Offer>,
    /// The workspaces this host governs, as the action service decides them: who may
    /// run an action here is what its workspace's policy grants them.
    workspaces: Arc<WorkspacesMap>,
    offered: std::sync::Mutex<Vec<Offered>>,
    log: std::sync::Mutex<InvocationLog<InvocationState>>,
    /// The workspaces' governance, once they open: what a member device asks of it here.
    governance: std::sync::OnceLock<std::sync::Weak<super::actions::Shared>>,
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

    /// Set what this host offers its workspaces.
    pub(crate) fn set_offer(&self, offer: Offer) {
        *self.offer.write_recover() = offer;
    }

    /// What this host offers its workspaces.
    pub(crate) fn offer(&self) -> Offer {
        *self.offer.read_recover()
    }

    /// The work this host answered the offer `invocation` for, made by the
    /// device `from`: what an invocation arriving here runs, or nothing when
    /// it names no offer of its own.
    fn offered_work(
        &self,
        from: [u8; 32],
        invocation: InvocationId,
        workspace: &str,
        route: &Route,
    ) -> Option<Work> {
        self.offered
            .lock_recover()
            .iter()
            .find(|offered| {
                offered.from == from
                    && offered.invocation == invocation
                    && offered.workspace == workspace
                    && offered.app == route.app
                    && offered.resource == route.resource
                    && offered.action == route.action
            })
            .map(|offered| offered.work.clone())
    }

    /// Remember `offer`'s work as this host's answer to it, replacing what an
    /// earlier answer for the same device and invocation said.
    fn remember(&self, from: [u8; 32], offer: &Query) {
        let offered = Offered {
            from,
            invocation: InvocationId(offer.request_id),
            workspace: offer.workspace.clone(),
            app: offer.route.app.clone(),
            resource: offer.route.resource.clone(),
            action: offer.route.action.clone(),
            work: offer.work.clone(),
        };
        let mut offers = self.offered.lock_recover();
        offers.retain(|held| !(held.from == from && held.invocation == offered.invocation));
        if offers.len() >= MAX_OFFERED {
            offers.remove(0);
        }
        offers.push(offered);
    }

    /// Whether the device `from` may run `route`'s action in `workspace` now:
    /// the workspace's per-action permission decides it, whatever kind of
    /// member the device belongs to.
    async fn permits(&self, from: [u8; 32], workspace: &str, route: &Route) -> bool {
        let held = self.workspaces.lock().await.get(workspace).cloned();
        let Some(held) = held else {
            return false;
        };
        let held = held.lock().await;
        held.peer()
            .permits_app_action(
                from,
                &route.app,
                &route.resource,
                &route.action,
                tilo_clock::wall(),
            )
            .unwrap_or(false)
    }
}

impl DeviceWork {
    /// Run the work offered here through `jobs`.
    pub(super) fn serve(&self, jobs: Arc<dyn HostJobs>) {
        *self.jobs.write_recover() = Some(jobs);
    }

    /// This host for the action service of the device that signs as `device`.
    ///
    /// # Panics
    ///
    /// Outside a runtime context, where there is nothing to answer on.
    fn new(workspaces: Arc<WorkspacesMap>) -> Arc<Self> {
        Arc::new(Self {
            jobs: std::sync::RwLock::default(),
            offer: std::sync::RwLock::default(),
            workspaces,
            offered: std::sync::Mutex::default(),
            log: std::sync::Mutex::default(),
            governance: std::sync::OnceLock::new(),
            runtime: tokio::runtime::Handle::current(),
        })
    }
}

/// The device action service of a host governing the workspaces in `workspaces`, as the
/// device whose signing key is `device`, running `jobs`.
///
/// # Panics
///
/// Outside a runtime context, where there is nothing to answer on.
pub(crate) fn action_service(
    workspaces: Arc<WorkspacesMap>,
    device: SigningKey,
    setting: WorkSetting,
) -> (ActionService, Arc<DeviceWork>) {
    let work = DeviceWork::new(Arc::clone(&workspaces));
    let action_service = ActionService::over(
        device,
        Arc::new(Answers(Arc::clone(&work))),
        Arc::new(GovernedWorkspaces(workspaces)),
    );
    action_service.offering(HostOffering {
        work: Arc::clone(&work),
        setting,
    });
    (action_service, work)
}

/// The workspaces this host governs, as the action service reads them: each is locked
/// for the read, so a roster change reaches the action service as it happens.
struct GovernedWorkspaces(Arc<WorkspacesMap>);

impl Workspaces for GovernedWorkspaces {
    fn read<'a>(
        &'a self,
        workspace: &'a str,
        read: &'a mut (dyn FnMut(&StatePeer) + Send),
    ) -> ReadFuture<'a> {
        Box::pin(async move {
            let held = self.0.lock().await.get(workspace).cloned();
            // A workspace this host does not govern is one it is no member of:
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

/// What a host says of itself when another device of one of its workspaces
/// offers it work.
///
/// It holds the workspace and is there to answer, and takes the work only when
/// its person's work setting does and the route's action declares a transform
/// it has the package for ([`HostJobs::runs`]); having taken it, it says which
/// of the inputs it already keeps, so the device that asks weighs moving them
/// against the run where the job was asked.
struct HostOffering {
    work: Arc<DeviceWork>,
    setting: WorkSetting,
}

impl Offering for HostOffering {
    fn report(&self, from: [u8; 32], whose: &Whose, offer: &Query) -> Answer {
        // What its person's entry says it offers decides first: a server set
        // not to run work declines it, whatever else it could run.
        if !self.work.offer().compute {
            return Answer::declining(Decline::NotTakingWork);
        }
        let jobs = self.work.jobs.read_recover().clone();
        let takes = jobs
            .as_ref()
            .is_some_and(|jobs| jobs.runs(&offer.workspace, &offer.work));
        let holds = match (&jobs, takes) {
            (Some(jobs), true) => {
                self.work.remember(from, offer);
                jobs.holds(&offer.workspace, &offer.work)
            }
            _ => Vec::new(),
        };
        self.setting.answer(
            whose,
            // A host is the machine that stays on: its power is external.
            Machine::here(Power::External),
            takes,
            // A host that is running as much as it takes on answers so now,
            // rather than winning a round it would refuse at run.
            jobs.as_ref().and_then(|jobs| jobs.free_slots()),
            holds,
        )
    }
}

/// The host's answers on the device action service: the work it was offered, run as a
/// compute job, and its workspaces' governance actions.
///
/// An app action reaches the host through the session its members run, which
/// carries the caller's certificate for the workspace's policy to decide. A
/// request that arrives here is run only for work this host answered an offer
/// for, or for the workspace's governance; anything else is refused as one this
/// device does not serve, so its caller learns at once instead of waiting.
pub(crate) struct Answers(pub(crate) Arc<DeviceWork>);

fn not_served() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "this host runs only the work it was offered on the device action service".into(),
    }
}

impl ActionProvider for Answers {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(not_served()))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if request.target.provider == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER {
            return self.0.govern(caller, request);
        }
        let Some(requester) = caller.device_public_key().copied() else {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        };
        let Some((workspace, app)) =
            tilo_actions::remote::application_provider(&request.target.provider)
        else {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        };
        let route = Route::new(
            app,
            request.target.address.resource.clone(),
            request.target.address.action.clone(),
        );
        // Unknown, not offered and not this device's to run are the same
        // answer: the invocation runs the offer it names, made by the device
        // that made it, and nothing else here.
        let Some(work) = self
            .0
            .offered_work(requester, request.invocation, workspace, &route)
        else {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        };
        let Some(jobs) = self.0.jobs.read_recover().clone() else {
            return Pending::ready(Err(not_served()));
        };
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
        }
        let caller = caller.clone();
        let workspace = workspace.to_owned();
        let shared = Arc::clone(&self.0);
        let (completer, answer) = tilo_actions::remote::pending();
        self.0.runtime.spawn(async move {
            // This host's compute is the workspace's: the permission for the
            // action decides who may run it here, whatever kind of
            // member their device belongs to.
            if !shared.permits(requester, &workspace, &route).await {
                completer.complete(Err(ActionCallError::NotPermitted));
                return;
            }
            let invocation = request.invocation;
            // An invocation this host already admitted is answered with what it
            // has, and is never run a second time.
            let admitted = {
                let mut log = shared.log.lock_recover();
                match log.reserve(&caller, &request) {
                    Err(error) => {
                        completer.complete(Err(error));
                        return;
                    }
                    Ok(Reservation::Admitted) => Some(
                        log.get(&caller, &invocation)
                            .map(InvocationState::receipt)
                            .ok_or(ActionCallError::UnknownInvocation),
                    ),
                    Ok(Reservation::Reserved) => None,
                }
            };
            if let Some(receipt) = admitted {
                completer.complete(receipt);
                return;
            }
            let output = jobs
                .run(&workspace, &work, requester, request)
                .await
                .unwrap_or(Err(ActionCallError::Unavailable {
                    reason: "this host stopped following the job".into(),
                }));
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
            let state = InvocationState::new(invocation, update);
            let receipt = state.receipt();
            shared.log.lock_recover().complete(&invocation, state);
            completer.complete(Ok(receipt));
        });
        answer
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
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

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        let Some(jobs) = self.0.jobs.read_recover().clone() else {
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
