//! Where a host's offloadable actions run: the action door's route
//! ([`tilo_actions::offload::ActionOffload`]) a host hands its broker as
//! `HostServices::action_offload`.
//!
//! Every host installs the offer round ([`offer_offload`], [`offer_door`]):
//! it asks the other devices of the invoking workspace whether they would run
//! the job sooner, weighing each answer against running it here, and sends the
//! invocation to the device that wins. A workspace whose other devices cannot
//! take the job — none connected, none running the app, none powered for it —
//! runs it here, through the app's own handler, and the door is told why.
//! [`OFFLOAD_ENV`] set to `off` installs no route at all.
//!
//! The round reads and keeps only the workspaces' own document media, through
//! the invoking app's scope in its workspace, so the inputs a job may receive
//! are exactly what the invoking workspace's apps could read, and the outputs
//! are read back by the same `tilo.content` addresses a local run returns
//! ([`RouteMedia`], [`RouteOutputs`]). A session that owns that store holds it
//! in process ([`crate::profile::MediaHost::store`]); the desktop session's
//! apps keep theirs in the filesystem daemon, which the route reaches over
//! the daemon's document-media socket as the session
//! ([`WorkspaceMedia`](tilo_relay::offload::WorkspaceMedia)).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use iroh::{Endpoint, EndpointAddr, EndpointId};
use neighbour_wire::offer::{Answer, MAX_WORK_INPUTS, Power, Route, State, Work};
use tilo_action_broker::record_content::{ContentState, RouteContent};
use tilo_action_broker::route::CrdtRoute;
use tilo_actions::offload::{ActionOffload, OffloadFuture, OffloadRequest, Offloaded};
use tilo_actions::remote::{InvokeRequest, Pending, pending};
use tilo_actions::{InvocationId, InvocationUpdate, Offload};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::actions::{ActionPlane, FOLLOW_DEADLINE, OFFER_DEADLINE, RemoteDevice};
use tilo_peer_state::offload::{OffloadPeers, RanAway};
use tilo_peer_state::placement::Choice;

// A host that keeps the workspaces' content itself implements the media a
// route binds its inputs from in the same scope the apps' own content is
// addressed in, and hands it to [`offer_offload`] beside the peers it asks.
pub use runtime_fs::blobs::BlobScope;
pub use tilo_relay::offload::{BindError, MediaFuture, WorkspaceMedia};

/// Set to `off` (or `0`, `false`, `no`) to keep every offloadable action on
/// this device even when a home relay could place it elsewhere.
pub const OFFLOAD_ENV: &str = "TILO_ACTION_OFFLOAD";
/// The route for offloadable invocations on this device over the offer round:
/// it asks the workspace's other devices whether they would run the job
/// sooner, through `peers` — a host's device plane, which asks as this device
/// and runs the invocation on the device that wins — and keeps the answer's
/// content where the invoking app reads its output.
///
/// The round is weighed with what this device can measure: the inputs' bytes
/// in `media`, through the invoking app's scope in its workspace, exactly as
/// the app would read them, and how long the same transform last took here.
/// Every input must be here, or the job runs here: content this device does
/// not hold is nothing it can weigh moving. `None` — every offloadable action
/// runs here, exactly as without offload — when offload is turned off.
#[must_use]
pub fn offer_offload(
    media: Arc<dyn WorkspaceMedia>,
    peers: Arc<dyn OffloadPeers>,
) -> Option<Arc<dyn ActionOffload>> {
    if disabled(std::env::var(OFFLOAD_ENV).ok().as_deref()) {
        tracing::info!("offloadable actions run on this device: offload is turned off");
        return None;
    }
    Some(Arc::new(OfferOffload {
        media,
        peers,
        ran: Mutex::default(),
        running: Mutex::default(),
        away: Mutex::default(),
    }))
}

/// The offer round as a host installs it: it asks `plane`'s workspaces' other
/// devices whether they would run the job sooner, reaching each at the
/// address its device key resolves to on this device's network, and reads and
/// keeps their content through `content` ([`RouteMedia`], [`RouteOutputs`]).
/// `runtime` carries the asks and the runs. `None` — every offloadable action
/// runs here — when offload is turned off.
#[must_use]
pub fn offer_door(
    plane: ActionPlane,
    endpoint: Endpoint,
    runtime: tokio::runtime::Handle,
    content: Arc<RouteContent>,
) -> Option<Arc<dyn ActionOffload>> {
    let peers = PlanePeers::new(
        plane,
        endpoint,
        |id: EndpointId| EndpointAddr::new(id),
        Arc::new(RouteOutputs::new(Arc::clone(&content))),
        runtime,
    );
    offer_offload(Arc::new(RouteMedia::new(content)), Arc::new(peers))
}

/// How long the content a device that ran an invocation produced has to reach
/// this device: it was produced for this workspace's app, which the invoking
/// device is a member of.
const CONTENT_WAIT: Duration = Duration::from_secs(10);

/// Where the content an offloaded run answered with is waited for: the bytes
/// of each content address the answer names, in the invoking app's scope in
/// its workspace.
pub trait OutputContent: Send + Sync {
    /// `digest` of `app`'s content in `workspace`, once it is here: what this
    /// device keeps already, else what the workspace's other devices hand over
    /// within `within`. `None` when it does not arrive in time.
    fn content<'a>(
        &'a self,
        workspace: &'a str,
        app: &'a str,
        digest: [u8; 32],
        within: Duration,
    ) -> MediaFuture<'a, Option<Vec<u8>>>;
}

/// The content this device shares with its workspaces already, as the record
/// content service its broker answers `wantBlob` through carries it: a device
/// that runs its workspaces' records through a host has no content plane of
/// its own beside it.
pub struct RouteOutputs(Arc<RouteContent>);

impl RouteOutputs {
    /// [`OutputContent`] over `content`, the route the profile's records'
    /// content travels on.
    #[must_use]
    pub fn new(content: Arc<RouteContent>) -> Self {
        Self(content)
    }
}

impl OutputContent for RouteOutputs {
    fn content<'a>(
        &'a self,
        workspace: &'a str,
        app: &'a str,
        digest: [u8; 32],
        within: Duration,
    ) -> MediaFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let hash = hex::encode(digest);
            let route = CrdtRoute::app(app, workspace);
            // Subscribed before it is asked for, so content that arrives at
            // once is not missed.
            let mut news = self.0.subscribe(&route);
            self.0.want(&route, &hash);
            let started = Instant::now();
            loop {
                let left = within.saturating_sub(started.elapsed());
                match tokio::time::timeout(left, news.recv()).await {
                    Ok(Some(arrived))
                        if arrived.hash == hash && arrived.state == ContentState::Resolved =>
                    {
                        return Some(arrived.bytes);
                    }
                    // Where another want of this scope got to: this one is
                    // still coming.
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => return None,
                }
            }
        })
    }
}

/// The record content a broker already routes, as the media an offer round
/// binds its inputs from and keeps its outputs in: the same service the
/// invoking app's `offerBlob` and `wantBlob` answer through, so an offloaded
/// invocation receives exactly what that app could read in its workspace
/// itself, and answers with content where the app's own runs put theirs.
pub struct RouteMedia(Arc<RouteContent>);

impl RouteMedia {
    /// [`WorkspaceMedia`] over `content`, the route this host's records' content
    /// travels on.
    #[must_use]
    pub fn new(content: Arc<RouteContent>) -> Self {
        Self(content)
    }
}

/// The record route of `scope`: the app's records in its workspace.
fn scope_route(scope: &BlobScope) -> CrdtRoute {
    CrdtRoute::app(scope.app(), scope.workspace())
}

impl WorkspaceMedia for RouteMedia {
    fn len<'a>(
        &'a self,
        scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<u64, BindError>> {
        Box::pin(async move {
            match self.0.kept(&scope_route(scope), &hex::encode(digest)) {
                Some(bytes) => Ok(bytes.len() as u64),
                None => Err(BindError::NotLocal),
            }
        })
    }

    fn read<'a>(
        &'a self,
        scope: &'a BlobScope,
        digest: [u8; 32],
    ) -> MediaFuture<'a, Result<Vec<u8>, BindError>> {
        Box::pin(async move {
            let bytes = self
                .0
                .kept(&scope_route(scope), &hex::encode(digest))
                .ok_or(BindError::NotLocal)?;
            if blake3::hash(&bytes).as_bytes() == &digest {
                Ok(bytes)
            } else {
                Err(BindError::Integrity)
            }
        })
    }

    fn keep<'a>(
        &'a self,
        scope: &'a BlobScope,
        bytes: Vec<u8>,
    ) -> MediaFuture<'a, Result<[u8; 32], String>> {
        Box::pin(async move {
            let digest = *blake3::hash(&bytes).as_bytes();
            self.0
                .offer(&scope_route(scope), &hex::encode(digest), &bytes)?;
            Ok(digest)
        })
    }
}

/// What this device says of itself when another device of one of its
/// workspaces offers it work: whether it runs `app`, how it is powered, how
/// much it has in hand, and which of the job's inputs it holds already — a
/// device that has them is not weighed as receiving them.
#[must_use]
pub fn own_offering(app: &str, holds: Vec<[u8; 32]>) -> Answer {
    let (critical_battery, power) = power();
    Answer {
        state: State {
            requester: false,
            online: true,
            app_installed: app_catalogue::apps::read_one_from_env(app)
                .is_ok_and(|row| row.is_some()),
            // Nothing here asks a device to keep off its discretionary work.
            data_saving: false,
            critical_battery,
            priority: 0,
            power,
            load_permille: load_permille(),
            // A session runs the work it is handed, one action at a time in
            // the app that asked: it keeps no queue to be full of.
            free_slots: None,
            latency_ms: 0,
        },
        holds,
    }
}

/// Battery share at or below which a device takes on nobody else's work while
/// it drains.
const CRITICAL_BATTERY_PERCENT: u8 = 10;

/// How this device is powered, and whether its battery is too low to take on
/// anyone else's work.
fn power() -> (bool, Power) {
    let snapshot = tilo_power_mgmt::sysfs::battery_snapshot();
    if !snapshot.present {
        return (false, Power::External);
    }
    let draining = matches!(
        snapshot.state,
        tilo_power_mgmt::BatteryState::Discharging
            | tilo_power_mgmt::BatteryState::PendingDischarge
    );
    let critical = draining && snapshot.percent <= CRITICAL_BATTERY_PERCENT;
    let power = match snapshot.state {
        tilo_power_mgmt::BatteryState::Charging | tilo_power_mgmt::BatteryState::PendingCharge => {
            Power::Charging
        }
        tilo_power_mgmt::BatteryState::Full => Power::External,
        tilo_power_mgmt::BatteryState::Discharging
        | tilo_power_mgmt::BatteryState::PendingDischarge => Power::Battery,
        tilo_power_mgmt::BatteryState::Empty | tilo_power_mgmt::BatteryState::Unknown => {
            Power::Unknown
        }
    };
    (critical, power)
}

/// What share of this device is already busy, per thousand, from the load the
/// machine reports against the processors it has.
fn load_permille() -> u32 {
    let cores =
        u32::try_from(std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get))
            .unwrap_or(u32::MAX);
    let Some(load) = std::fs::read_to_string("/proc/loadavg").ok() else {
        return 0;
    };
    let Some(one) = load.split_whitespace().next() else {
        return 0;
    };
    // The kernel reports the one-minute load in hundredths; read it as
    // thousandths so that dividing by the processors stays whole numbers.
    let (whole, fraction) = one.split_once('.').unwrap_or((one, ""));
    let whole: u32 = whole.parse().unwrap_or(0);
    let mut fraction = fraction.chars().take(3).collect::<String>();
    while fraction.len() < 3 {
        fraction.push('0');
    }
    let fraction: u32 = fraction.parse().unwrap_or(0);
    let thousandths = whole.saturating_mul(1_000).saturating_add(fraction);
    thousandths.checked_div(cores.max(1)).unwrap_or(0)
}

/// Whether a device of a workspace is connected here now, as the round asks
/// it.
type Reachable = Arc<dyn Fn(&str, [u8; 32]) -> bool + Send + Sync>;

/// [`OffloadPeers`] over a device plane: the round asks the workspace's other
/// devices through `plane` — who may be asked is what the plane's own roster
/// says — and the invocation runs on the device that wins it through the same
/// connection, with the content it produced waited for through `outputs`.
///
/// The plane asks as this device and at `endpoint`; `addresses` says where
/// each device the plane resolves is reached. Nothing here carries authority:
/// the device that wins decides the invocation through its own dispatcher.
pub struct PlanePeers {
    plane: ActionPlane,
    endpoint: Endpoint,
    addresses: Arc<dyn Fn(EndpointId) -> EndpointAddr + Send + Sync>,
    /// Which devices of a workspace are connected here now: the round asks
    /// those, and one that is away is not waited for.
    reachable: Reachable,
    outputs: Arc<dyn OutputContent>,
    runtime: tokio::runtime::Handle,
}

impl PlanePeers {
    /// [`OffloadPeers`] over `plane`, asked at `endpoint`, with every device
    /// reached at the address `addresses` gives for it, and what an answer
    /// produced waited for through `outputs`. `runtime` carries the asks and
    /// the runs, never the thread that invokes.
    #[must_use]
    pub fn new(
        plane: ActionPlane,
        endpoint: Endpoint,
        addresses: impl Fn(EndpointId) -> EndpointAddr + Send + Sync + 'static,
        outputs: Arc<dyn OutputContent>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            plane,
            endpoint,
            addresses: Arc::new(addresses),
            reachable: Arc::new(|_, _| true),
            outputs,
            runtime,
        }
    }

    /// Ask only the devices of a workspace that `reachable` says are
    /// connected here now, in the workspace named: a device that is away is
    /// not asked, and the round does not wait for it.
    #[must_use]
    pub fn reachable(
        self,
        reachable: impl Fn(&str, [u8; 32]) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            reachable: Arc::new(reachable),
            ..self
        }
    }

    /// Where `id` is reached.
    fn addr(&self, id: EndpointId) -> EndpointAddr {
        (self.addresses)(id)
    }
}

impl OffloadPeers for PlanePeers {
    fn ask(
        &self,
        workspace: &str,
        route: &Route,
        work: &Work,
        invocation: InvocationId,
        within: Duration,
    ) -> Pending<Choice> {
        let (complete, answer) = pending();
        let (plane, endpoint, runtime) = (
            self.plane.clone(),
            self.endpoint.clone(),
            self.runtime.clone(),
        );
        let addresses = Arc::clone(&self.addresses);
        let reachable = Arc::clone(&self.reachable);
        let (workspace, route, work) = (workspace.to_owned(), route.clone(), work.clone());
        runtime.spawn(async move {
            // Only the devices that are connected here now: one that is away
            // has nothing to answer with, and waiting for it costs the run
            // here more than it could ever save.
            let targets: Vec<EndpointAddr> = plane
                .participants(&workspace)
                .await
                .into_iter()
                .filter(|key| reachable(&workspace, *key))
                .filter_map(|key| EndpointId::from_bytes(&key).ok())
                .map(|id| addresses(id))
                .collect();
            let chosen = plane
                .offers(
                    &endpoint, targets, &workspace, &route, &work, invocation, within,
                )
                .await;
            // A round the device that asked stopped waiting for is not chosen
            // on: nothing would run there.
            if let Some(chosen) = chosen {
                complete.complete(chosen);
            }
        });
        answer
    }

    fn run(
        &self,
        workspace: &str,
        device: [u8; 32],
        invocation: InvokeRequest,
    ) -> Pending<Result<RanAway, String>> {
        let (complete, answer) = pending();
        let Ok(id) = EndpointId::from_bytes(&device) else {
            complete.complete(Err("that is not a device key".into()));
            return answer;
        };
        let remote = Arc::new(self.plane.remote(&self.endpoint, &self.addr(id), workspace));
        let outputs = Arc::clone(&self.outputs);
        let workspace = workspace.to_owned();
        self.runtime.spawn(async move {
            let ran = run_away(outputs.as_ref(), &workspace, device, remote, invocation).await;
            complete.complete(ran);
        });
        answer
    }
}

/// Run the person's `invocation` on `remote` and follow it until it settles,
/// with the bytes of the content it produced in `workspace`.
async fn run_away(
    outputs: &dyn OutputContent,
    workspace: &str,
    device: [u8; 32],
    remote: Arc<RemoteDevice>,
    invocation: InvokeRequest,
) -> Result<RanAway, String> {
    // The app whose content the output is: the one whose action was invoked,
    // as the device that ran it kept what it produced.
    let Some((_, app)) = tilo_actions::remote::application_provider(&invocation.target.provider)
    else {
        return Err("the invocation names no app, so its output content cannot be read".into());
    };
    let app = app.to_owned();
    let output = match remote
        .invoke_until_settled(invocation, FOLLOW_DEADLINE)
        .await
    {
        Some(Ok(InvocationUpdate::Succeeded { output, .. })) => output,
        Some(Ok(InvocationUpdate::Failed { error, .. })) => return Err(error.message),
        Some(Ok(InvocationUpdate::Cancelled { message, .. })) => {
            return Err(message.unwrap_or_else(|| "the action was cancelled there".into()));
        }
        Some(Ok(update)) => {
            return Err(format!("the action there did not settle: {update:?}"));
        }
        Some(Err(error)) => return Err(error.to_string()),
        None => return Err("the device stopped following the invocation".into()),
    };
    let mut produced = Vec::new();
    for address in &output.resources {
        let Some(named) = tilo_actions::content_hash(address) else {
            continue;
        };
        let mut digest = [0; 32];
        hex::decode_to_slice(named, &mut digest)
            .map_err(|_| "the output names a malformed content address".to_owned())?;
        let Some(bytes) = outputs.content(workspace, &app, digest, CONTENT_WAIT).await else {
            return Err("the content it produced did not reach this device".into());
        };
        produced.push(bytes);
    }
    Ok(RanAway {
        device,
        output,
        outputs: produced,
    })
}

/// Local runs awaited at once before the oldest are forgotten: an invocation
/// that expires never reports its run.
const MAX_LOCAL_RUNS: usize = 64;

/// How long one transform last took where it was asked, over how many input
/// bytes: what moving a job's inputs has to beat.
#[derive(Clone, Copy)]
struct Run {
    elapsed: Duration,
    input_bytes: u64,
}

impl Run {
    /// This run's wall time scaled to `input_bytes` of input, in whole
    /// milliseconds: the transforms offloaded are batch work whose cost grows
    /// with what they read.
    fn ms(self, input_bytes: u64) -> u64 {
        let millis = self.elapsed.as_millis();
        let scaled = if self.input_bytes == 0 {
            millis
        } else {
            millis
                .saturating_mul(u128::from(input_bytes))
                .div_ceil(u128::from(self.input_bytes))
        };
        u64::try_from(scaled).unwrap_or(u64::MAX)
    }
}

/// Offloadable invocations of this device's workspaces, run where the offer
/// round picks.
struct OfferOffload {
    media: Arc<dyn WorkspaceMedia>,
    peers: Arc<dyn OffloadPeers>,
    /// How long each transform last took where it was asked, by
    /// `package:entrypoint`.
    ran: Mutex<HashMap<String, Run>>,
    /// Input bytes of the invocations handed to the app to run here, by
    /// workspace and inputs, until the door reports that run.
    running: Mutex<HashMap<(String, Vec<String>), u64>>,
    /// The invocations this device sent to another device, and where: one that
    /// comes back is not sent away again.
    away: Mutex<HashMap<InvocationId, [u8; 32]>>,
}

impl OfferOffload {
    /// Where `request` runs: on the device the round picks, or here, with why.
    async fn place(&self, request: OffloadRequest, invocation: Option<InvokeRequest>) -> Offloaded {
        let started = Instant::now();
        // Only a route handed the invocation can send it to another device:
        // without it there is no request to send, so the app runs this one.
        let Some(invocation) = invocation else {
            return Offloaded::RunHere {
                reason: "the invocation was not handed to the route".into(),
            };
        };
        let scope = match BlobScope::new(request.workspace.clone(), request.app_id.clone()) {
            Ok(scope) => scope,
            Err(error) => {
                return Offloaded::RunHere {
                    reason: format!("the workspace's content is not addressable: {error}"),
                };
            }
        };
        let (work, input_bytes) = match self.work(&request, &scope).await {
            Ok(work) => work,
            Err(reason) => {
                tracing::info!(app = %request.offload.app, %reason, "offloadable action runs here");
                return Offloaded::RunHere { reason };
            }
        };
        let reason = match self
            .away(&request, &scope, &work, invocation, started)
            .await
        {
            Ok(ran) => return ran,
            Err(reason) => reason,
        };
        {
            let mut running = self.running.lock_recover();
            // A local run the door never reports (it failed) is forgotten
            // rather than kept forever.
            if running.len() >= MAX_LOCAL_RUNS {
                running.clear();
            }
            running.insert(
                (request.workspace.clone(), request.inputs.clone()),
                input_bytes,
            );
        }
        tracing::info!(
            app = %request.offload.app,
            entrypoint = %request.offload.entrypoint,
            %reason,
            "offloadable action runs here"
        );
        Offloaded::RunHere { reason }
    }

    /// What the round is asked: `request`'s inputs as this device's store
    /// measures them, each distinct digest once, and how long the same
    /// transform last took here, scaled to their bytes.
    async fn work(
        &self,
        request: &OffloadRequest,
        scope: &BlobScope,
    ) -> Result<(Work, u64), String> {
        let mut inputs = Vec::with_capacity(request.inputs.len().min(MAX_WORK_INPUTS));
        let mut input_bytes = 0_u64;
        for (index, hex) in request.inputs.iter().enumerate() {
            let mut digest = [0; 32];
            hex::decode_to_slice(hex, &mut digest)
                .map_err(|_| format!("input {index} is not a content hash"))?;
            if inputs.iter().any(|(named, _)| *named == digest) {
                continue;
            }
            let bytes = self
                .media
                .len(scope, digest)
                .await
                .map_err(|error| format!("input {index}: {error}"))?;
            input_bytes = input_bytes.saturating_add(bytes);
            if inputs.len() < MAX_WORK_INPUTS {
                inputs.push((digest, bytes));
            }
        }
        let local_wall_ms = self
            .ran
            .lock_recover()
            .get(&transform(&request.offload))
            .map(|ran| ran.ms(input_bytes));
        Ok((
            Work {
                inputs,
                input_bytes,
                local_wall_ms,
                offload: Some(request.offload.clone()),
            },
            input_bytes,
        ))
    }

    /// Ask the round and run the invocation on the device that wins it, or say
    /// why this device runs it instead.
    async fn away(
        &self,
        request: &OffloadRequest,
        scope: &BlobScope,
        work: &Work,
        invocation: InvokeRequest,
        started: Instant,
    ) -> Result<Offloaded, String> {
        let sent_to = self
            .away
            .lock_recover()
            .get(&invocation.invocation)
            .copied();
        if let Some(device) = sent_to {
            let mut short = hex::encode(device);
            short.truncate(8);
            return Err(format!(
                "the invocation is already running on the device {short} sent it to"
            ));
        }
        let route = Route::new(
            request.app_id.clone(),
            request.address.resource.clone(),
            request.address.action.clone(),
        );
        // The round only pays for itself when there is a run here to beat:
        // one that was never measured could never lose to a device that is
        // quicker, so asking is the cost of the round and nothing else. And
        // it is never waited on longer than the run here takes.
        let Some(local_ms) = work.local_wall_ms else {
            return Err("nothing is quicker than a run it knows nothing of".into());
        };
        let within = OFFER_DEADLINE.min(Duration::from_millis(local_ms));
        let choice = self
            .peers
            .ask(
                &request.workspace,
                &route,
                work,
                invocation.invocation,
                within,
            )
            .await
            .ok_or("the offer round was abandoned before it finished")?;
        let Some(worker) = choice.worker else {
            return Err(choice.to_string());
        };
        self.away
            .lock_recover()
            .insert(invocation.invocation, worker);
        let ran = self
            .peers
            .run(&request.workspace, worker, invocation.clone())
            .await;
        self.away.lock_recover().remove(&invocation.invocation);
        let ran: RanAway = match ran {
            Some(ran) => ran.map_err(|reason| {
                let mut short = hex::encode(worker);
                short.truncate(8);
                format!("{short} did not run it: {reason}")
            })?,
            None => return Err("the device that won the round did not answer".into()),
        };
        self.keep(scope, &ran).await?;
        Ok(Offloaded::Ran {
            output: ran.output,
            device: hex::encode(worker),
            elapsed: started.elapsed(),
        })
    }

    /// Keep what the device that ran the invocation produced as this
    /// workspace's app content, in the order it named: the app that invoked the
    /// action here reads its output by the addresses a local run returns, and
    /// bytes that are not what the device named never become that output.
    async fn keep(&self, scope: &BlobScope, ran: &RanAway) -> Result<(), String> {
        let mut produced = ran.outputs.iter();
        for address in &ran.output.resources {
            let Some(named) = tilo_actions::content_hash(address) else {
                continue;
            };
            let Some(bytes) = produced.next() else {
                return Err("the answer names more content than the device sent".into());
            };
            let kept = self
                .media
                .keep(scope, bytes.clone())
                .await
                .map_err(|error| format!("the answer was not kept here: {error}"))?;
            if hex::encode(kept) != named {
                return Err("the answer's bytes are not the content it names".into());
            }
        }
        if produced.next().is_some() {
            return Err("the answer's content is not what it names".into());
        }
        Ok(())
    }
}

impl ActionOffload for OfferOffload {
    fn offload(&self, request: OffloadRequest) -> OffloadFuture<'_> {
        Box::pin(self.place(request, None))
    }

    fn offload_invocation(
        &self,
        request: OffloadRequest,
        invocation: InvokeRequest,
    ) -> OffloadFuture<'_> {
        Box::pin(self.place(request, Some(invocation)))
    }

    fn ran_here(&self, request: &OffloadRequest, elapsed: Duration) {
        let Some(input_bytes) = self
            .running
            .lock_recover()
            .remove(&(request.workspace.clone(), request.inputs.clone()))
        else {
            return;
        };
        self.ran.lock_recover().insert(
            transform(&request.offload),
            Run {
                elapsed,
                input_bytes,
            },
        );
    }
}

/// The transform `offload` declares, as placement knows it:
/// `package:entrypoint`, which is what an action's cost belongs to.
fn transform(offload: &Offload) -> String {
    format!("{}:{}", offload.app, offload.entrypoint)
}

/// Whether `value` of [`OFFLOAD_ENV`] turns offload off.
fn disabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        )
    })
}

#[cfg(test)]
#[path = "offload_offer_tests.rs"]
mod offer_tests;
