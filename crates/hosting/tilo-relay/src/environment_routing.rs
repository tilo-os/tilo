//! The relay's side of environment sessions: admitting members, choosing a
//! worker, and splicing streams.
//!
//! A member's environment stream ([`StreamOpen::Environment`]) is admitted
//! by the workspace's current membership, then spliced frame by frame onto a
//! stream the relay opens to a worker on the worker's own compute
//! connection. The relay remembers which worker hosts each session and whose
//! it is, so a re-attach from any of the member's devices reaches the same
//! guest and a re-attach from anyone else is refused before a worker sees
//! it.
//!
//! A legacy launch arrives from the worker hosting the session
//! ([`StreamOpen::LegacyLaunch`]); the relay sends it to a worker that
//! advertises `kvm` and relays that app's window frames to the member's
//! legacy-windows streams, and the member's input back.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use iroh::endpoint::{RecvStream, SendStream};
use tilo_clock::unix_now_ms;
use tilo_relay_protocol::environment::{
    ENVIRONMENT_PROTOCOL_VERSION, LaunchId, LegacyFrame, LegacyLaunch, LegacyLaunchOutcome,
    MAX_ENVIRONMENT_FRAME_BYTES, MemberIdentity, Refusal, ServerFrame, SessionId, SessionRequest,
    StreamOpen, WindowFrame, WindowInput, WorkerStreamOpen, bounded_detail,
};
use tilo_user_identity::RevocationList;
use tokio::io::AsyncWrite;
use tokio::sync::mpsc;

use crate::auth::AuthenticatedPeer;
use crate::environment_host::{
    EnvironmentStreamError, MeterEvent, Metering, read_message, write_message,
};
use crate::frame::{read_frame_or_eof, write_frame};
use crate::mesh::DeviceKey;
use crate::peer::WorkspaceRegistry;
use crate::policy::TrustPolicy;
use crate::workspace::{SharedWorkspace, WorkspaceRuntime};

const VIEWER_QUEUE: usize = 128;
const LAUNCH_INPUT_QUEUE: usize = 64;

/// One workspace's routed environment sessions and legacy launches.
#[derive(Default)]
pub struct WorkspaceEnvironments {
    sessions: HashMap<SessionId, SessionRoute>,
    launches: HashMap<LaunchId, LaunchRoute>,
    viewers: HashMap<[u8; 32], Vec<Viewer>>,
    next_viewer: u64,
}

#[derive(Clone, Debug)]
struct SessionRoute {
    worker: DeviceKey,
    owner: MemberIdentity,
}

struct LaunchRoute {
    member: MemberIdentity,
    input: mpsc::Sender<WindowInput>,
}

struct Viewer {
    id: u64,
    frames: mpsc::Sender<WindowFrame>,
}

impl WorkspaceEnvironments {
    /// Sessions routed in this workspace.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Whether `session` is routed, and to which worker for whom.
    #[must_use]
    pub fn session_route(&self, session: &SessionId) -> Option<(DeviceKey, MemberIdentity)> {
        self.sessions
            .get(session)
            .map(|route| (route.worker, route.owner))
    }

    /// Legacy launches running in this workspace.
    #[must_use]
    pub fn launch_count(&self) -> usize {
        self.launches.len()
    }

    fn add_viewer(&mut self, user: [u8; 32], frames: mpsc::Sender<WindowFrame>) -> u64 {
        let id = self.next_viewer;
        self.next_viewer = id.wrapping_add(1);
        self.viewers
            .entry(user)
            .or_default()
            .push(Viewer { id, frames });
        id
    }

    fn broadcast(&mut self, user: &[u8; 32], frame: &WindowFrame) {
        if let Some(viewers) = self.viewers.get_mut(user) {
            // A stalled viewer misses frames rather than stalling the app;
            // the next keyframe group recovers it.
            viewers.retain(|viewer| {
                !matches!(
                    viewer.frames.try_send(frame.clone()),
                    Err(mpsc::error::TrySendError::Closed(_))
                )
            });
            if viewers.is_empty() {
                self.viewers.remove(user);
            }
        }
    }
}

/// Relay-wide state the environment routes need.
#[derive(Clone)]
pub(crate) struct RoutingContext {
    pub(crate) workspaces: WorkspaceRegistry,
    pub(crate) own_user_public_key: [u8; 32],
    pub(crate) trust_policy: Arc<RwLock<TrustPolicy>>,
    pub(crate) revocations: Arc<RwLock<RevocationList>>,
    pub(crate) metering: Arc<dyn Metering>,
}

impl RoutingContext {
    /// The peer is still admitted relay-wide: not revoked, and admitted by
    /// the live trust policy.
    fn still_trusted(&self, peer: &AuthenticatedPeer) -> bool {
        self.revocations
            .read()
            .is_ok_and(|list| !list.is_revoked(&peer.device_public_key))
            && self
                .trust_policy
                .read()
                .is_ok_and(|policy| policy.admits(&peer.cert, &self.own_user_public_key))
    }

    async fn workspace(&self, workspace_id: u64) -> Option<SharedWorkspace> {
        self.workspaces.lock().await.get(&workspace_id).cloned()
    }
}

/// Whether `peer` is a current member of `workspace`: an exact member
/// device, a device of an admitted user, or, on a same-owner host that
/// admits its owner's devices, one of the owner's devices.
pub(crate) fn admitted(
    workspace: &WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    own_user_public_key: &[u8; 32],
) -> bool {
    workspace.membership.contains(&peer.device_public_key)
        || workspace.admitted_users.contains(&peer.user_public_key)
        || (workspace.admit_authenticated
            && !workspace.governed_access
            && &peer.user_public_key == own_user_public_key)
}

fn member_of(peer: &AuthenticatedPeer) -> MemberIdentity {
    MemberIdentity {
        user: peer.user_public_key,
        device: peer.device_public_key,
    }
}

fn random_id() -> [u8; 16] {
    let mut id = [0; 16];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut id);
    id
}

async fn refuse<W>(
    send: &mut W,
    reason: Refusal,
    detail: &str,
) -> Result<(), EnvironmentStreamError>
where
    W: AsyncWrite + Unpin + Send,
{
    write_message(
        send,
        &ServerFrame::Refused {
            reason,
            detail: bounded_detail(detail),
        },
    )
    .await
}

/// Serve one stream a peer opened on its compute connection.
pub(crate) async fn serve_stream(
    context: RoutingContext,
    peer: AuthenticatedPeer,
    mut send: SendStream,
    mut receive: RecvStream,
) -> Result<(), EnvironmentStreamError> {
    let Some(open) = read_message::<_, StreamOpen>(&mut receive).await? else {
        return Ok(());
    };
    if !context.still_trusted(&peer) {
        refuse(
            &mut send,
            Refusal::NotAMember,
            "this device is no longer admitted",
        )
        .await?;
        let _ = send.finish();
        return Ok(());
    }
    let outcome = match open {
        StreamOpen::Environment {
            version,
            workspace_id,
            request,
        } => {
            if version == ENVIRONMENT_PROTOCOL_VERSION {
                route_environment(
                    &context,
                    &peer,
                    workspace_id,
                    request,
                    &mut send,
                    &mut receive,
                )
                .await
            } else {
                refuse(
                    &mut send,
                    Refusal::UnsupportedVersion,
                    "unsupported version",
                )
                .await
            }
        }
        StreamOpen::LegacyWindows {
            version,
            workspace_id,
        } => {
            if version == ENVIRONMENT_PROTOCOL_VERSION {
                serve_viewer(&context, &peer, workspace_id, &mut send, &mut receive).await
            } else {
                refuse(
                    &mut send,
                    Refusal::UnsupportedVersion,
                    "unsupported version",
                )
                .await
            }
        }
        StreamOpen::LegacyLaunch {
            version,
            workspace_id,
            session,
            request,
        } => {
            let outcome = if version == ENVIRONMENT_PROTOCOL_VERSION {
                route_legacy_launch(&context, &peer, workspace_id, session, request).await
            } else {
                LegacyLaunchOutcome::Refused {
                    reason: Refusal::UnsupportedVersion,
                    detail: "unsupported version".into(),
                }
            };
            write_message(&mut send, &outcome).await
        }
    };
    let _ = send.finish();
    outcome
}

async fn route_environment(
    context: &RoutingContext,
    peer: &AuthenticatedPeer,
    workspace_id: u64,
    request: SessionRequest,
    send: &mut SendStream,
    receive: &mut RecvStream,
) -> Result<(), EnvironmentStreamError> {
    if !request.valid() {
        return refuse(send, Refusal::InvalidRequest, "invalid environment request").await;
    }
    let Some(workspace) = context.workspace(workspace_id).await else {
        return refuse(
            send,
            Refusal::NotAMember,
            "workspace not bound on this relay",
        )
        .await;
    };
    let member = member_of(peer);
    let chosen = {
        let mut workspace = workspace.lock().await;
        choose_route(
            &mut workspace,
            peer,
            &context.own_user_public_key,
            workspace_id,
            &request,
        )
    };
    let (session, worker, connection, opened) = match chosen {
        Ok(chosen) => chosen,
        Err((reason, detail)) => return refuse(send, reason, detail).await,
    };
    let forget = || async {
        workspace
            .lock()
            .await
            .compute
            .environments
            .sessions
            .remove(&session);
    };
    let Ok((mut worker_send, mut worker_receive)) = connection.open_bi().await else {
        if opened {
            forget().await;
        }
        return refuse(send, Refusal::NoWorker, "the worker could not be reached").await;
    };
    write_message(
        &mut worker_send,
        &WorkerStreamOpen::Environment {
            version: ENVIRONMENT_PROTOCOL_VERSION,
            workspace_id,
            member,
            request,
            session,
        },
    )
    .await?;
    tracing::debug!(
        session = %hex::encode(session),
        worker = %hex::encode(worker),
        "environment stream routed"
    );
    let ended = splice(send, receive, &mut worker_send, &mut worker_receive).await;
    if ended.unwrap_or(false) {
        forget().await;
    }
    Ok(())
}

/// Admit `peer` and pick where its request goes: a new session on the
/// least-loaded environment worker, or the worker already hosting the
/// session it re-attaches to. Returns the session, the worker, its
/// connection, and whether the route is new.
fn choose_route(
    workspace: &mut WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    own_user_public_key: &[u8; 32],
    workspace_id: u64,
    request: &SessionRequest,
) -> Result<(SessionId, DeviceKey, iroh::endpoint::Connection, bool), (Refusal, &'static str)> {
    if !admitted(workspace, peer, own_user_public_key) {
        return Err((Refusal::NotAMember, "not a member of this workspace"));
    }
    let member = member_of(peer);
    let compute = &mut workspace.compute;
    match request {
        SessionRequest::Open { record, .. } => {
            if record.workspace_id != workspace_id {
                return Err((Refusal::InvalidRequest, "record is in another workspace"));
            }
            let (worker, connection) = compute.environment_worker(unix_now_ms()).ok_or((
                Refusal::NoWorker,
                "no worker on this relay runs environments right now",
            ))?;
            let session = random_id();
            compute.environments.sessions.insert(
                session,
                SessionRoute {
                    worker,
                    owner: member,
                },
            );
            Ok((session, worker, connection, true))
        }
        SessionRequest::Attach { session, .. } => {
            let route = compute
                .environments
                .sessions
                .get(session)
                .cloned()
                .ok_or((Refusal::UnknownSession, "no such session"))?;
            if route.owner.user != member.user {
                return Err((Refusal::NotOwner, "that session belongs to someone else"));
            }
            let connection = compute.worker_connection(&route.worker).ok_or((
                Refusal::NoWorker,
                "the worker running this session is offline",
            ))?;
            Ok((*session, route.worker, connection, false))
        }
    }
}

/// Splice a member's stream onto a worker's. Returns whether the session
/// is gone (it exited, or the worker refused it).
async fn splice(
    member_send: &mut SendStream,
    member_receive: &mut RecvStream,
    worker_send: &mut SendStream,
    worker_receive: &mut RecvStream,
) -> Result<bool, EnvironmentStreamError> {
    let upstream = async {
        while let Some(frame) =
            read_frame_or_eof(member_receive, MAX_ENVIRONMENT_FRAME_BYTES).await?
        {
            write_frame(worker_send, &frame).await?;
        }
        let _ = worker_send.finish();
        Ok::<(), EnvironmentStreamError>(())
    };
    let downstream = async {
        while let Some(frame) =
            read_frame_or_eof(worker_receive, MAX_ENVIRONMENT_FRAME_BYTES).await?
        {
            let gone = matches!(
                postcard::from_bytes::<ServerFrame>(&frame),
                Ok(ServerFrame::Exited { .. }
                    | ServerFrame::Refused {
                        reason: Refusal::UnknownSession
                            | Refusal::LaunchFailed
                            | Refusal::Quota(_)
                            | Refusal::InvalidRequest
                            | Refusal::NoWorker,
                        ..
                    })
            );
            write_frame(member_send, &frame).await?;
            if gone {
                return Ok::<bool, EnvironmentStreamError>(true);
            }
        }
        Ok(false)
    };
    tokio::pin!(upstream);
    tokio::pin!(downstream);
    let mut upstream_done = false;
    loop {
        tokio::select! {
            result = &mut upstream, if !upstream_done => {
                upstream_done = true;
                if result.is_err() {
                    // The member went away; the session stays on the worker,
                    // which sees its stream end and detaches.
                    return Ok(false);
                }
            }
            result = &mut downstream => return result,
        }
    }
}

async fn serve_viewer(
    context: &RoutingContext,
    peer: &AuthenticatedPeer,
    workspace_id: u64,
    send: &mut SendStream,
    receive: &mut RecvStream,
) -> Result<(), EnvironmentStreamError> {
    let Some(workspace) = context.workspace(workspace_id).await else {
        return refuse(
            send,
            Refusal::NotAMember,
            "workspace not bound on this relay",
        )
        .await;
    };
    let member = member_of(peer);
    let (frames_tx, mut frames) = mpsc::channel(VIEWER_QUEUE);
    let mut guard = workspace.lock().await;
    if !admitted(&guard, peer, &context.own_user_public_key) {
        drop(guard);
        return refuse(send, Refusal::NotAMember, "not a member of this workspace").await;
    }
    let viewer_id = guard
        .compute
        .environments
        .add_viewer(member.user, frames_tx);
    drop(guard);
    write_message(send, &WindowFrame::Watching).await?;
    let outcome = async {
        loop {
            tokio::select! {
                frame = frames.recv() => {
                    let Some(frame) = frame else { return Ok(()) };
                    write_message(send, &frame).await?;
                }
                input = read_message::<_, WindowInput>(receive) => {
                    let Some(input) = input? else { return Ok(()) };
                    if !input.valid() {
                        continue;
                    }
                    let target = {
                        let workspace = workspace.lock().await;
                        workspace
                            .compute
                            .environments
                            .launches
                            .get(&input.launch)
                            .filter(|launch| launch.member.user == member.user)
                            .map(|launch| launch.input.clone())
                    };
                    if let Some(target) = target {
                        let _ = target.try_send(input);
                    }
                }
            }
        }
    }
    .await;
    let mut workspace = workspace.lock().await;
    if let Some(viewers) = workspace.compute.environments.viewers.get_mut(&member.user) {
        viewers.retain(|viewer| viewer.id != viewer_id);
    }
    drop(workspace);
    outcome
}

async fn route_legacy_launch(
    context: &RoutingContext,
    peer: &AuthenticatedPeer,
    workspace_id: u64,
    session: SessionId,
    request: LegacyLaunch,
) -> LegacyLaunchOutcome {
    let refused = |reason, detail: &str| LegacyLaunchOutcome::Refused {
        reason,
        detail: bounded_detail(detail),
    };
    if peer.user_public_key != context.own_user_public_key
        || !peer.cert.capabilities.can_act_as_host
    {
        return refused(
            Refusal::NotOwner,
            "only this relay's workers launch legacy apps",
        );
    }
    if !request.valid() {
        return refused(Refusal::InvalidRequest, "invalid legacy launch");
    }
    let Some(workspace) = context.workspace(workspace_id).await else {
        return refused(Refusal::UnknownSession, "workspace not bound on this relay");
    };
    let (member, kvm, connection) = {
        let workspace = workspace.lock().await;
        let compute = &workspace.compute;
        let Some(route) = compute
            .environments
            .sessions
            .get(&session)
            .filter(|route| route.worker == peer.device_public_key)
        else {
            drop(workspace);
            return refused(
                Refusal::UnknownSession,
                "the session is not hosted by this worker",
            );
        };
        let member = route.owner;
        let Some((kvm, connection)) = compute.kvm_worker(unix_now_ms()) else {
            drop(workspace);
            return refused(
                Refusal::NoWorker,
                "no worker on this relay runs legacy apps",
            );
        };
        (member, kvm, connection)
    };
    let launch = random_id();
    let Ok((mut kvm_send, kvm_receive)) = connection.open_bi().await else {
        return refused(Refusal::NoWorker, "the legacy worker could not be reached");
    };
    let app = request.app.clone();
    if write_message(
        &mut kvm_send,
        &WorkerStreamOpen::Legacy {
            version: ENVIRONMENT_PROTOCOL_VERSION,
            workspace_id,
            member,
            launch,
            request,
        },
    )
    .await
    .is_err()
    {
        return refused(Refusal::NoWorker, "the legacy worker could not be reached");
    }
    let (input_tx, input) = mpsc::channel(LAUNCH_INPUT_QUEUE);
    workspace.lock().await.compute.environments.launches.insert(
        launch,
        LaunchRoute {
            member,
            input: input_tx,
        },
    );
    tracing::debug!(
        launch = %hex::encode(launch),
        worker = %hex::encode(kvm),
        "legacy launch routed"
    );
    tokio::spawn(pump_launch(
        LaunchPump {
            workspace,
            metering: Arc::clone(&context.metering),
            member,
            launch,
            session,
            app,
        },
        kvm_send,
        kvm_receive,
        input,
    ));
    LegacyLaunchOutcome::Launched { launch }
}

struct LaunchPump {
    workspace: SharedWorkspace,
    metering: Arc<dyn Metering>,
    member: MemberIdentity,
    launch: LaunchId,
    session: SessionId,
    app: String,
}

/// Carry one legacy launch: the KVM worker's frames to the member's
/// viewers, the member's input to the KVM worker, usage to metering.
async fn pump_launch(
    pump: LaunchPump,
    mut kvm_send: SendStream,
    mut kvm_receive: RecvStream,
    mut input: mpsc::Receiver<WindowInput>,
) {
    let LaunchPump {
        workspace,
        metering,
        member,
        launch,
        session,
        app,
    } = pump;
    let code = loop {
        tokio::select! {
            frame = read_message::<_, LegacyFrame>(&mut kvm_receive) => {
                let Ok(Some(frame)) = frame else { break None };
                let window = match frame {
                    LegacyFrame::Started => WindowFrame::Launched {
                        launch,
                        session,
                        app: app.clone(),
                    },
                    LegacyFrame::Window(frame) => WindowFrame::Window { launch, frame },
                    LegacyFrame::Usage(usage) => {
                        metering.record(MeterEvent::LegacyUsage { member, usage });
                        continue;
                    }
                    LegacyFrame::Exited { code } => break code,
                    LegacyFrame::Refused { .. } => break None,
                };
                workspace
                    .lock()
                    .await
                    .compute
                    .environments
                    .broadcast(&member.user, &window);
            }
            event = input.recv() => {
                let Some(event) = event else { break None };
                if write_message(&mut kvm_send, &event).await.is_err() {
                    break None;
                }
            }
        }
    };
    let mut guard = workspace.lock().await;
    let environments = &mut guard.compute.environments;
    environments.launches.remove(&launch);
    environments.broadcast(&member.user, &WindowFrame::Exited { launch, code });
    drop(guard);
    let _ = kvm_send.finish();
}
