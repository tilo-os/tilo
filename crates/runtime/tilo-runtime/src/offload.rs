//! Where a host's offloadable actions run: the action API's route
//! ([`tilo_actions::offload::ActionOffload`]) a host hands its broker as
//! `ProcessServices::action_offload`.
//!
//! The route asks the other devices of the invoking workspace it reaches to
//! take the invocation's turn (`tilo.work#take`,
//! [`tilo_workspace_service::work`]). Each decides for itself whether it may
//! take it and claims it with a change to the workspace; the earliest claim
//! wins. The device that took the turn runs the transform the action declares
//! and answers with its output, which the route follows; every other device
//! answers why not. The invocation runs here, through the app's own handler,
//! when no device takes it within [`CLAIM_DEADLINE`], none is reachable, the
//! one that took it stops answering. Whether a device may take a turn is
//! for it alone to decide, never for the route that asks.
//! [`OFFLOAD_ENV`] set to `off` installs no route at all.
//!
//! The outputs are read back by the same `tilo.content` addresses a local run
//! returns and kept through the invoking app's scope in its workspace
//! ([`RouteMedia`], [`RouteOutputs`]). A session that owns that store holds it
//! in process ([`crate::profile::MediaHost::store`]); the desktop session's
//! apps keep theirs in the filesystem daemon, which the route reaches over the
//! daemon's document-media socket as the session ([`WorkspaceMedia`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt as _;
use futures_util::stream::FuturesUnordered;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_actions::offload::{ActionOffload, OffloadFuture, OffloadRequest, Offloaded};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvocationReceipt, InvokeRequest, Pending,
    pending,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_peer_state::actions::{ActionService, FOLLOW_DEADLINE, RemoteDevice};
use tilo_workspace_service::apps::route::CrdtRoute;
use tilo_workspace_service::content::{ContentState, RouteContent};
use tilo_workspace_service::work::Job;

// A host that keeps the workspaces' content itself implements the media a
// route keeps outputs in, in the same scope the apps' own content is
// addressed in, and hands it to [`turn_offload`] beside the devices it asks.
pub use media::{MediaFuture, WorkspaceMedia};
pub use runtime_fs::blobs::BlobScope;

mod media;

/// Set to `off` (or `0`, `false`, `no`) to keep every offloadable action on
/// this device even when another device could take it.
pub const OFFLOAD_ENV: &str = "TILO_ACTION_OFFLOAD";

/// How long the devices asked have to claim a turn before it runs here.
pub const CLAIM_DEADLINE: Duration = Duration::from_secs(2);

/// The route for offloadable invocations on this device: it asks `takers` to
/// take each one's turn, and keeps the content the device that took it
/// produced in `media`, where the invoking app reads its output. `None` when
/// offload is turned off: every offloadable action runs here.
#[must_use]
pub fn turn_offload(
    media: Arc<dyn WorkspaceMedia>,
    takers: Arc<dyn Takers>,
) -> Option<Arc<dyn ActionOffload>> {
    if disabled(std::env::var(OFFLOAD_ENV).ok().as_deref()) {
        tracing::info!("offloadable actions run on this device: offload is turned off");
        return None;
    }
    Some(Arc::new(TurnOffload { media, takers }))
}

/// [`turn_offload`] as a host installs it: it asks `action_service`'s
/// workspaces' other devices, reached at the address their device key
/// resolves to on this device's network, and reads and keeps their content
/// through `content`. `runtime` carries the asks.
#[must_use]
pub fn turn_route(
    action_service: ActionService,
    endpoint: Endpoint,
    runtime: tokio::runtime::Handle,
    content: Arc<RouteContent>,
) -> Option<Arc<dyn ActionOffload>> {
    let takers = ActionServicePeers::new(
        action_service,
        endpoint,
        EndpointAddr::new,
        Arc::new(RouteOutputs::new(Arc::clone(&content))),
        runtime,
    );
    turn_offload(Arc::new(RouteMedia::new(content)), Arc::new(takers))
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
/// that runs its workspaces' records through a host has no content store of
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

/// A turn another device took and ran: its output, and the bytes of the
/// content the output names, in order.
pub struct Taken {
    /// The device that ran it.
    pub device: [u8; 32],
    /// What it answered.
    pub output: ActionOutput,
    /// The bytes of each content address `output` names.
    pub outputs: Vec<Vec<u8>>,
}

/// The devices an offloaded invocation's turn is asked of.
pub trait Takers: Send + Sync {
    /// Ask the devices of `workspace` to take `job` as the turn `invocation`,
    /// and follow the one that takes it to its output; or say why none did.
    fn take(
        &self,
        workspace: &str,
        job: Job,
        invocation: InvocationId,
    ) -> Pending<Result<Taken, String>>;
}

/// Whether a device of a workspace is connected here now.
type Reachable = Arc<dyn Fn(&str, [u8; 32]) -> bool + Send + Sync>;

/// [`Takers`] over a device action service: the workspace's other devices
/// its roster names are asked through it, as this device, and the one that
/// takes the turn is followed through the same connection, with the content it
/// produced waited for through `outputs`.
#[derive(Clone)]
pub struct ActionServicePeers {
    action_service: ActionService,
    endpoint: Endpoint,
    addresses: Arc<dyn Fn(EndpointId) -> EndpointAddr + Send + Sync>,
    /// Which devices of a workspace are connected here now: only those are
    /// asked.
    reachable: Reachable,
    outputs: Arc<dyn OutputContent>,
    runtime: tokio::runtime::Handle,
}

impl ActionServicePeers {
    /// [`Takers`] over `action_service`, asked at `endpoint`, with every
    /// device reached at the address `addresses` gives for it, and the content
    /// a turn produced waited for through `outputs`. `runtime` carries the
    /// asks, never the thread that invokes.
    #[must_use]
    pub fn new(
        action_service: ActionService,
        endpoint: Endpoint,
        addresses: impl Fn(EndpointId) -> EndpointAddr + Send + Sync + 'static,
        outputs: Arc<dyn OutputContent>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            action_service,
            endpoint,
            addresses: Arc::new(addresses),
            reachable: Arc::new(|_, _| true),
            outputs,
            runtime,
        }
    }

    /// Ask only the devices of a workspace that `reachable` says are
    /// connected here now.
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

    async fn take_turn(
        &self,
        workspace: &str,
        job: Job,
        invocation: InvocationId,
    ) -> Result<Taken, String> {
        let request = job.take_request(invocation);
        // The device's own signature is what the asked device reads, not this.
        let caller = Caller::local_person("device");
        let asked: Vec<_> = self
            .action_service
            .key_holders(workspace)
            .await
            .into_iter()
            .filter(|key| (self.reachable)(workspace, *key))
            .filter_map(|key| EndpointId::from_bytes(&key).ok())
            .map(|id| {
                let remote = Arc::new(self.action_service.remote(
                    &self.endpoint,
                    &(self.addresses)(id),
                    workspace,
                ));
                let asking = remote.invoke(&caller, request.clone());
                (remote, asking)
            })
            .collect();
        if asked.is_empty() {
            return Err("no other device of the workspace is reachable".into());
        }
        let remote = first_to_take(asked, CLAIM_DEADLINE).await?;
        let device = remote.device();
        let output = follow(remote, request).await.map_err(|reason| {
            let mut short = hex::encode(device);
            short.truncate(8);
            format!("{short} took it and did not finish: {reason}")
        })?;
        let mut outputs = Vec::new();
        for address in &output.resources {
            let Some(named) = tilo_actions::content_hash(address) else {
                continue;
            };
            let mut digest = [0; 32];
            hex::decode_to_slice(named, &mut digest)
                .map_err(|_| "the output names a malformed content address".to_owned())?;
            let bytes = self
                .outputs
                .content(workspace, &job.app, digest, CONTENT_WAIT)
                .await
                .ok_or("the content it produced did not reach this device")?;
            outputs.push(bytes);
        }
        Ok(Taken {
            device,
            output,
            outputs,
        })
    }
}

impl Takers for ActionServicePeers {
    fn take(
        &self,
        workspace: &str,
        job: Job,
        invocation: InvocationId,
    ) -> Pending<Result<Taken, String>> {
        let (complete, answer) = pending();
        let (peers, workspace) = (self.clone(), workspace.to_owned());
        self.runtime.spawn(async move {
            complete.complete(peers.take_turn(&workspace, job, invocation).await);
        });
        answer
    }
}

/// The first of `asked` to take the turn, as each answers: the one whose
/// claim held. Every other answers why not, and one that does not answer
/// within `within` is not waited for.
async fn first_to_take<T>(
    asked: Vec<(T, Pending<Result<InvocationReceipt, ActionCallError>>)>,
    within: Duration,
) -> Result<T, String> {
    let mut answers: FuturesUnordered<_> = asked
        .into_iter()
        .map(|(device, asking)| async move { (device, asking.await) })
        .collect();
    let mut why = Vec::new();
    let deadline = tokio::time::sleep(within);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => {
                return Err(format!("no device took it within {} ms", within.as_millis()));
            }
            answer = answers.next() => match answer {
                Some((device, Some(Ok(_)))) => return Ok(device),
                Some((_, Some(Err(ActionCallError::Refused { reason })))) => why.push(reason),
                Some((_, Some(Err(error)))) => why.push(error.to_string()),
                Some((_, None)) => why.push("a device did not answer".into()),
                None => return Err(why.join("; ")),
            },
        }
    }
}

/// Follow the turn `request` asked `remote` to take until it settles there.
async fn follow(remote: Arc<RemoteDevice>, request: InvokeRequest) -> Result<ActionOutput, String> {
    match remote.invoke_until_settled(request, FOLLOW_DEADLINE).await {
        Some(Ok(InvocationUpdate::Succeeded { output, .. })) => Ok(output),
        Some(Ok(InvocationUpdate::Failed { error, .. })) => Err(error.message),
        Some(Ok(InvocationUpdate::Cancelled { message, .. })) => {
            Err(message.unwrap_or_else(|| "it was cancelled there".into()))
        }
        Some(Ok(update)) => Err(format!("it did not settle: {update:?}")),
        Some(Err(error)) => Err(error.to_string()),
        None => Err("the device stopped answering".into()),
    }
}

/// Offloadable invocations of this device's workspaces, run on the device
/// that takes their turn, or here.
struct TurnOffload {
    media: Arc<dyn WorkspaceMedia>,
    takers: Arc<dyn Takers>,
}

impl TurnOffload {
    /// Where `request`, the invocation `invocation`, runs: on the device that
    /// takes its turn, or here, with why.
    async fn place(&self, request: OffloadRequest, invocation: InvocationId) -> Offloaded {
        let started = Instant::now();
        match self.away(&request, invocation, started).await {
            Ok(ran) => ran,
            Err(reason) => {
                tracing::info!(
                    app = %request.offload.app,
                    entrypoint = %request.offload.entrypoint,
                    %reason,
                    "offloadable action runs here"
                );
                Offloaded::RunHere { reason }
            }
        }
    }

    async fn away(
        &self,
        request: &OffloadRequest,
        invocation: InvocationId,
        started: Instant,
    ) -> Result<Offloaded, String> {
        let scope = BlobScope::new(request.workspace.clone(), request.app_id.clone())
            .map_err(|error| format!("the workspace's content is not addressable: {error}"))?;
        let mut inputs = Vec::with_capacity(request.inputs.len());
        for (index, hex) in request.inputs.iter().enumerate() {
            let mut digest = [0; 32];
            hex::decode_to_slice(hex, &mut digest)
                .map_err(|_| format!("input {index} is not a content hash"))?;
            if !inputs.contains(&digest) {
                inputs.push(digest);
            }
        }
        let job = Job {
            app: request.app_id.clone(),
            resource: request.address.resource.clone(),
            action: request.address.action.clone(),
            offload: request.offload.clone(),
            inputs,
        };
        let taken = self
            .takers
            .take(&request.workspace, job, invocation)
            .await
            .ok_or("the devices asked were given up on")??;
        self.keep(&scope, &taken).await?;
        Ok(Offloaded::Ran {
            output: taken.output,
            device: hex::encode(taken.device),
            elapsed: started.elapsed(),
        })
    }

    /// Keep what the device that took the turn produced as this workspace's
    /// app content, in the order it named: the app that invoked the action
    /// here reads its output by the addresses a local run returns, and bytes
    /// that are not what the device named never become that output.
    async fn keep(&self, scope: &BlobScope, taken: &Taken) -> Result<(), String> {
        let mut produced = taken.outputs.iter();
        for address in &taken.output.resources {
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

impl ActionOffload for TurnOffload {
    fn offload(&self, request: OffloadRequest) -> OffloadFuture<'_> {
        Box::pin(self.place(request, InvocationId::new()))
    }

    fn offload_invocation(
        &self,
        request: OffloadRequest,
        invocation: InvokeRequest,
    ) -> OffloadFuture<'_> {
        Box::pin(self.place(request, invocation.invocation))
    }
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
#[path = "offload_turn_tests.rs"]
mod turn_tests;
