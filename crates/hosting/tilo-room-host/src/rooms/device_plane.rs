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
//! App actions are not answered here: one reaches the host through the
//! session its members run, where the caller's certificate and the
//! workspace's policy decide it
//! ([`AppServingPlane`](crate::app_serving::AppServingPlane)).

use std::collections::BTreeMap;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use neighbour_wire::offer::{Answer, Power, Route, State, Work};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending,
};
use tilo_peer_state::StatePeer;
use tilo_peer_state::actions::{ActionPlane, Offering, ReadFuture, Workspaces};
use tokio::sync::Mutex as AsyncMutex;

use super::GovernedRoom;

/// The workspaces a host governs, by workspace: what [`Rooms`](super::Rooms)
/// keeps and this plane reads.
pub(crate) type RoomsMap = AsyncMutex<BTreeMap<String, Arc<AsyncMutex<GovernedRoom>>>>;

/// The device plane of a host governing the workspaces in `rooms`, as the
/// device whose signing key is `device`.
///
/// # Panics
///
/// Outside a runtime context, where there is nothing to answer on.
pub(crate) fn plane(rooms: Arc<RoomsMap>, device: SigningKey) -> ActionPlane {
    let plane = ActionPlane::over(
        device,
        Arc::new(NoAppActions),
        Arc::new(GovernedRooms(rooms)),
    );
    plane.offering(HostOffering);
    plane
}

/// The workspaces this host governs, as the plane reads them: each is locked
/// for the read, so a roster change reaches the plane as it happens.
struct GovernedRooms(Arc<RoomsMap>);

impl Workspaces for GovernedRooms {
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
            read(held.peer());
            true
        })
    }
}

/// What a host says of itself when another device of one of its workspaces
/// offers it work.
///
/// It holds the workspace and is there to answer. It runs no app actions on the
/// device plane, so it says it cannot run the app asked for: a host never
/// claims work it cannot run. Once it does run them there, this is where it
/// reports its standing — how it is powered, and the inputs it already keeps
/// — so the device that asks can weigh moving them against the run where the
/// job was asked.
struct HostOffering;

impl Offering for HostOffering {
    fn report(&self, _workspace: &str, _route: &Route, _work: &Work) -> Answer {
        Answer {
            state: State {
                requester: false,
                online: true,
                holds_workspace: true,
                app_installed: false,
                // It holds the workspace's state as a member, not as a store of
                // ciphertext it has no keys for.
                store_role: false,
                data_saving: false,
                critical_battery: false,
                priority: 0,
                power: Power::Unknown,
                load_permille: load_permille(),
                latency_ms: 0,
            },
            holds: Vec::new(),
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
    // optional-decode: a load this machine cannot read is none in hand.
    let Ok(load) = one.parse::<f32>() else {
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

/// The host answers no app actions on the device plane: an app's action
/// reaches it through the session its members run, which carries the
/// caller's certificate for the workspace's policy to decide. A request that
/// arrives here anyway is refused as one this device does not serve, so its
/// caller learns at once instead of waiting.
struct NoAppActions;

fn not_served() -> ActionHostError {
    ActionHostError::Unavailable {
        reason: "this host answers no app actions on the device plane".into(),
    }
}

impl ActionHost for NoAppActions {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        Pending::ready(Err(not_served()))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        _request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        Pending::ready(Err(not_served()))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        Pending::ready(Err(not_served()))
    }
}
