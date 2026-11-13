//! The device plane: what another device of a workspace asks this host
//! directly, with no member session in between.
//!
//! A host is a member device of the workspaces it governs: its own device
//! holds their governance and their state. So those workspaces answer the
//! devices' plane (`tilo-actions/1`) as any other member device does. A device
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
//! on the same plane as a compute job over content it keeps. Every other app
//! action is not answered here: those reach the host through the session its
//! members run, where the caller's certificate and the workspace's policy
//! decide them ([`AppServingPlane`](crate::app_serving::AppServingPlane)).

use std::collections::BTreeMap;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use neighbour_wire::offer::{Answer, Power, Query, Route, State, Work};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationLedger,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, Reservation,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_peer_state::StatePeer;
use tilo_peer_state::actions::{ActionPlane, Offering, ReadFuture, Workspaces};
use tokio::sync::Mutex as AsyncMutex;

use super::GovernedWorkspace;

/// The workspaces a host governs, by workspace: what [`Workspaces`](super::Workspaces)
/// keeps and this plane reads.
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
    ) -> Pending<Result<ActionOutput, ActionHostError>>;

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

/// How many offers this host remembers at once. A device asks and then
/// invokes at once; what it is not invoked on within this many later offers
/// is forgotten.
const MAX_OFFERED: usize = 256;

/// The jobs a host runs, shared by the offering that answers offers and the
/// dispatcher that answers invocations, and the work offered here.
///
/// The plane is built before any embedder hands the host a job runner, so
/// [`Self::serve`] fills it in. A host without one says it runs no app
/// actions, as it always did.
pub(crate) struct HostWork {
    jobs: std::sync::RwLock<Option<Arc<dyn HostJobs>>>,
    /// The workspaces this host governs, as the plane decides them: who may
    /// run an action here is what its workspace's policy grants them.
    workspaces: Arc<WorkspacesMap>,
    offered: std::sync::Mutex<Vec<Offered>>,
    ledger: std::sync::Mutex<InvocationLedger<InvocationState>>,
    runtime: tokio::runtime::Handle,
}

impl HostWork {
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
    /// collaborator the device belongs to.
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

impl HostWork {
    /// Run the work offered here through `jobs`.
    pub(super) fn serve(&self, jobs: Arc<dyn HostJobs>) {
        *self.jobs.write_recover() = Some(jobs);
    }

    /// This host for the plane of the device that signs as `device`.
    ///
    /// # Panics
    ///
    /// Outside a runtime context, where there is nothing to answer on.
    fn new(workspaces: Arc<WorkspacesMap>) -> Arc<Self> {
        Arc::new(Self {
            jobs: std::sync::RwLock::default(),
            workspaces,
            offered: std::sync::Mutex::default(),
            ledger: std::sync::Mutex::default(),
            runtime: tokio::runtime::Handle::current(),
        })
    }
}

/// The device plane of a host governing the workspaces in `workspaces`, as the
/// device whose signing key is `device`, running `jobs`.
///
/// # Panics
///
/// Outside a runtime context, where there is nothing to answer on.
pub(crate) fn plane(
    workspaces: Arc<WorkspacesMap>,
    device: SigningKey,
) -> (ActionPlane, Arc<HostWork>) {
    let work = HostWork::new(Arc::clone(&workspaces));
    let plane = ActionPlane::over(
        device,
        Arc::new(HostActions(Arc::clone(&work))),
        Arc::new(GovernedWorkspaces(workspaces)),
    );
    plane.offering(HostOffering(Arc::clone(&work)));
    (plane, work)
}

/// The workspaces this host governs, as the plane reads them: each is locked
/// for the read, so a roster change reaches the plane as it happens.
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
/// It holds the workspace, is there to answer, and runs on external power as
/// the always-on server it is. It takes the work only when the route's action
/// declares a transform and it has the package that performs it
/// ([`HostJobs::runs`]); having taken it, it says which of the inputs it
/// already keeps, so the device that asks weighs moving them against the run
/// where the job was asked.
struct HostOffering(Arc<HostWork>);

impl Offering for HostOffering {
    fn report(&self, from: [u8; 32], offer: &Query) -> Answer {
        let jobs = self.0.jobs.read_recover().clone();
        let takes = jobs
            .as_ref()
            .is_some_and(|jobs| jobs.runs(&offer.workspace, &offer.work));
        let holds = match (&jobs, takes) {
            (Some(jobs), true) => {
                self.0.remember(from, offer);
                jobs.holds(&offer.workspace, &offer.work)
            }
            _ => Vec::new(),
        };
        Answer {
            state: State {
                requester: false,
                online: true,
                app_installed: takes,
                data_saving: false,
                critical_battery: false,
                priority: 0,
                power: Power::External,
                load_permille: load_permille(),
                // A host that is running as much as it takes on answers so
                // now, rather than winning a round it would refuse at run.
                free_slots: jobs.as_ref().and_then(|jobs| jobs.free_slots()),
                latency_ms: 0,
            },
            holds,
        }
    }
}

/// What share of this machine is already busy, per thousand: the load the
/// kernel reports against the processors it has. A machine that reports no
/// load says none is in hand.
fn load_permille() -> u32 {
    let cores =
        u32::try_from(std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get))
            .unwrap_or(u32::MAX);
    let Ok(report) = std::fs::read_to_string("/proc/loadavg") else {
        return 0;
    };
    let Some(one) = report.split_whitespace().next() else {
        return 0;
    };
    let Ok(load) = tilo_failure::ignored!(
        "a load this machine cannot read is none in hand",
        one.parse::<f32>()
    ) else {
        return 0;
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a load is a small non-negative figure, clamped to whole thousandths"
    )]
    let thousandths = (load.clamp(0.0, 4_000_000.0) * 1_000.0) as u32;
    thousandths.checked_div(cores.max(1)).unwrap_or(0)
}

/// The host's answers on the device plane: the work it was offered, run as a
/// compute job, and nothing else.
///
/// An app action reaches the host through the session its members run, which
/// carries the caller's certificate for the workspace's policy to decide. A
/// request that arrives here is run only for work this host answered an offer
/// for; anything else is refused as one this device does not serve, so its
/// caller learns at once instead of waiting.
struct HostActions(Arc<HostWork>);

fn not_served() -> ActionHostError {
    ActionHostError::Unavailable {
        reason: "this host runs only the work it was offered on the device plane".into(),
    }
}

impl ActionHost for HostActions {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        Pending::ready(Err(not_served()))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let Some(requester) = caller.device_public_key().copied() else {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        };
        let Some((workspace, app)) =
            tilo_actions::remote::application_provider(&request.target.provider)
        else {
            return Pending::ready(Err(ActionHostError::UnknownAction));
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
            return Pending::ready(Err(ActionHostError::UnknownAction));
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
            // collaborator their device belongs to.
            if !shared.permits(requester, &workspace, &route).await {
                completer.complete(Err(ActionHostError::NotPermitted));
                return;
            }
            let invocation = request.invocation;
            // An invocation this host already admitted is answered with what it
            // has, and is never run a second time.
            let admitted = {
                let mut ledger = shared.ledger.lock_recover();
                match ledger.reserve(&caller, &request) {
                    Err(error) => {
                        completer.complete(Err(error));
                        return;
                    }
                    Ok(Reservation::Admitted) => Some(
                        ledger
                            .get(&caller, &invocation)
                            .map(InvocationState::receipt)
                            .ok_or(ActionHostError::UnknownInvocation),
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
                .unwrap_or(Err(ActionHostError::Unavailable {
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
            shared.ledger.lock_recover().complete(&invocation, state);
            completer.complete(Ok(receipt));
        });
        answer
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.0.ledger.lock_recover().get(caller, id).cloned()
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        let Some(jobs) = self.0.jobs.read_recover().clone() else {
            return Pending::ready(Err(not_served()));
        };
        match self.0.ledger.lock_recover().get(caller, id) {
            Some(state) if state.is_terminal() => {
                Pending::ready(Err(ActionHostError::NotCancellable {
                    reason: "the job already finished here".into(),
                }))
            }
            Some(_) => {
                jobs.cancel(*id);
                Pending::ready(Ok(()))
            }
            None => Pending::ready(Err(ActionHostError::UnknownInvocation)),
        }
    }
}
