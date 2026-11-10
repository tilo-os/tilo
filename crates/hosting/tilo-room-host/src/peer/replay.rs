//! Replay writing: resume logs, the replay catch-up a subscriber's backlog
//! runs under, writing replay batches and boundaries to the wire, and
//! authorizing each state frame before it is written.

use super::{
    Arc, AsyncWriteExt, AuthenticatedPeer, Envelope, Invariant, PeerError, ReplayAdvance,
    ReplayHandle, ReplayTicket, SessionCtx, SharedWorkspace, StateSource, Step, StoreError,
    WorkspaceOperation, WrittenBatch, current_session, encode_charged, test_point,
    trace_native_publication, unix_now_ns, workspace_access,
};
use tilo_lock::MutexRecover as _;

/// What a resuming subscriber has received: the frames this connection wrote
/// to it are appended to its resume log, and a [`Envelope::Position`] naming
/// the log's length follows them.
pub(super) struct ResumeLog {
    pub(super) store: crate::store::WorkspaceStore,
    pub(super) issuer: crate::position::PositionIssuer,
    pub(super) device: [u8; 32],
    pub(super) generation: [u8; 16],
    pub(super) resume: crate::position::Resume,
}

impl ResumeLog {
    /// Record `frames` as delivered; returns the position covering them.
    fn record<'f>(&mut self, frames: impl IntoIterator<Item = &'f Envelope>) -> Envelope {
        let digests: Vec<[u8; 32]> = frames
            .into_iter()
            .filter(|frame| matches!(frame, Envelope::Delta { .. }))
            .filter_map(crate::store::resume::frame_digest)
            .collect();
        if !digests.is_empty() {
            match self
                .store
                .append_resume(&self.device, &self.resume.log, &digests)
            {
                Ok(()) => {
                    self.resume.len = self.resume.len.saturating_add(digests.len() as u64);
                }
                // A position never covers what the log did not record; the
                // device replays those frames once more on its next resume.
                Err(error) => tracing::warn!(%error, "resume log append failed"),
            }
        }
        self.issuer.envelope(
            self.store.workspace_id(),
            self.device,
            self.generation,
            self.resume,
        )
    }
}

/// Record frames written to `workspace_id` in its resume log, if it has one,
/// and return the position to write after them.
pub(super) fn resume_record<'f>(
    session: &SessionCtx<'_>,
    workspace_id: u64,
    frames: impl IntoIterator<Item = &'f Envelope>,
) -> Option<Envelope> {
    let mut resumes = session.resumes.lock_recover();
    resumes.get_mut(&workspace_id).map(|log| log.record(frames))
}

pub(super) struct ReplayWrite {
    pub(super) ticket: Option<(u64, ReplayTicket, ReplayHandle, Arc<ReplayCatchup>)>,
    pub(super) reply: Option<Envelope>,
    pub(super) replaying: bool,
}
pub(super) async fn authorize_state_frame(
    frame: &Envelope,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<(), PeerError> {
    let Some((id, subscribed)) = state_frame_room(frame) else {
        return Ok(());
    };
    authorize_state_recipient(id, subscribed, peer, session).await
}

/// The workspace a state frame goes to, and whether it needs this connection to
/// be the workspace's current subscriber; `None` for frames that need neither.
const fn state_frame_room(frame: &Envelope) -> Option<(u64, bool)> {
    match frame {
        Envelope::Delta { workspace_id, .. }
        | Envelope::Ready { workspace_id, .. }
        | Envelope::WorkspaceBootstrap { workspace_id, .. }
        | Envelope::RetiredPresence { workspace_id, .. } => Some((*workspace_id, true)),
        Envelope::SyncResponse { workspace_id, .. } => Some((*workspace_id, false)),
        _ => None,
    }
}

/// [`authorize_state_frame`] for a frame to workspace `id`; `subscribed` frames
/// (deltas, presence, `Ready`) also need this connection to be the workspace's
/// current subscriber.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the membership guard spans the whole authorization so it observes one roster generation"
)]
pub(super) async fn authorize_state_recipient(
    id: u64,
    subscribed: bool,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<(), PeerError> {
    let ws = session
        .workspaces
        .lock()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| PeerError::Protocol("workspace disappeared".into()))?;
    let wr = crate::workspace_io::fresh_membership(&ws).await?;
    if !session.admits(&wr, peer, WorkspaceOperation::State)
        || ((subscribed || wr.subscribers.contains_key(&peer.device_public_key))
            && !current_session(&wr, peer, session.outbound))
    {
        return Err(PeerError::Protocol(
            "state recipient authorization changed".into(),
        ));
    }
    Ok(())
}
/// Write the workspace's bootstrap for `peer` ahead of its replay
/// ([`Envelope::WorkspaceBootstrap`]), in a workspace this relay governs or hosts for
/// its governor: what a member device builds its replica from before it
/// admits the replay. A host attached to the workspace keeps its own replica and
/// gets none; this host's own device takes one wherever it
/// connects, so that the app-serving peer of a host that may act as host
/// decides its workspaces like any other member.
async fn write_bootstrap<W: tokio::io::AsyncWrite + Unpin>(
    workspace_id: u64,
    ws: &SharedWorkspace,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    write: &mut W,
) -> Result<(), PeerError> {
    if peer.device_public_key != *session.own_device_public_key
        && ws.lock().await.attached(&peer.device_public_key)
    {
        return Ok(());
    }
    let (authority, hosted) = {
        let runtime = ws.lock().await;
        (
            runtime.workspace_authority.clone(),
            runtime.hosted_replica.clone(),
        )
    };
    let bootstrap = match authority.and_then(|authority| {
        authority
            .workspaces()
            .map(|workspaces| (workspaces, authority))
    }) {
        Some((workspaces, authority)) => {
            workspaces
                .subscriber_bootstrap(authority.workspace(), &peer.cert)
                .await
        }
        None => hosted.and_then(|replica| replica.bootstrap(workspace_id, &peer.cert)),
    };
    let Some(bootstrap) = bootstrap else {
        return Ok(());
    };
    let envelope = Envelope::WorkspaceBootstrap {
        workspace_id,
        bootstrap,
    };
    authorize_state_frame(&envelope, peer, session).await?;
    write
        .write_all(&encode_charged(&envelope, session, false)?)
        .await?;
    Ok(())
}

/// Frames addressed to a replaying device that wait for the replay's next
/// pause between batches.
const MAX_ADDRESSED: usize = 64;

/// Write the frames committed for this device alone while its replay runs
/// ([`ReplayCatchup::address`]), each authorised as every state frame is.
async fn write_addressed<W: tokio::io::AsyncWrite + Unpin>(
    catchup: &ReplayCatchup,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    write: &mut W,
) -> Result<(), PeerError> {
    for frame in catchup.take_addressed() {
        authorize_state_frame(&frame, peer, session).await?;
        write
            .write_all(&encode_charged(&frame, session, false)?)
            .await?;
    }
    Ok(())
}

/// One subscription's replay as [`finish_replay_write`] drives it: the workspace,
/// its persistence owner, and the replay ticket's authority and catch-up.
#[derive(Clone, Copy)]
struct ReplayRun<'r> {
    workspace_id: u64,
    workspace: &'r SharedWorkspace,
    owner: &'r crate::persistence::Persistence,
    authority: &'r Arc<crate::persistence::ReplayAuthority>,
    catchup: &'r Arc<ReplayCatchup>,
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the workspace and authority handles are cloned into the boundary closures that outlive their last named use"
)]
pub(super) async fn finish_replay_write<W: tokio::io::AsyncWrite + Unpin>(
    batch: ReplayWrite,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    pending: &std::sync::atomic::AtomicUsize,
    write: &mut W,
) -> Result<(), PeerError> {
    let mut reply = batch.reply;
    let mut completed_replay = None;
    if let Some((workspace_id, ticket, handle, catchup)) = batch.ticket {
        let ws = session
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| PeerError::Protocol("replay workspace disappeared".into()))?;
        let owner = crate::persistence::handle(&ws).await;
        let run = ReplayRun {
            workspace_id,
            workspace: &ws,
            owner: &owner,
            authority: &ticket.authority,
            catchup: &catchup,
        };
        write_bootstrap(workspace_id, &ws, peer, session, write).await?;
        let replaying = crate::timing::start();
        let replayed_frames = drive_replay(run, handle, peer, session, write).await?;
        crate::timing::event("join.replay_total", replaying, replayed_frames);
        authorize_replay(&ws, &StateSource::new(peer, session), &ticket.authority).await?;
        reply = Some(Envelope::Ready {
            workspace_id,
            pending_deltas: u32::try_from(replayed_frames).unwrap_or(u32::MAX),
        });
        completed_replay = Some((
            catchup.clone(),
            ws.clone(),
            StateSource::new(peer, session),
            ticket,
        ));
        // Dropping this ticket schedules specific bounded spool reclamation.
    }
    if let Some(reply) = reply {
        let readying = crate::timing::start();
        authorize_state_frame(&reply, peer, session).await?;
        if let Some((catchup, workspace, source, ticket)) = &completed_replay {
            test_point!(pause(workspace workspace, "replay-before-ready")
                on crate::persistence::handle(workspace).await.workers());
            authorize_replay_boundary(workspace, source, &ticket.authority, catchup).await?;
            if !ticket.authority.bound_active() {
                return Err(PeerError::Protocol(
                    "replay session fenced before Ready".into(),
                ));
            }
        }
        write
            .write_all(&encode_charged(&reply, session, false)?)
            .await?;
        if let Envelope::Ready { workspace_id, .. } = &reply
            && let Some(position) = resume_record(session, *workspace_id, [])
        {
            write
                .write_all(&encode_charged(&position, session, false)?)
                .await?;
        }
        if let Some((catchup, workspace, source, ticket)) = completed_replay {
            complete_replay_ready(&workspace, &source, &ticket.authority, &catchup).await?;
            crate::timing::event("join.ready", readying, 0);
        }
    }
    if batch.replaying {
        pending.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
    Ok(())
}

/// Write `run`'s replay to the connection batch by batch, then cut what
/// committed while it ran and replay that too, until the subscriber is
/// caught up. Returns how many frames the replays held.
async fn drive_replay<W: tokio::io::AsyncWrite + Unpin>(
    run: ReplayRun<'_>,
    mut handle: ReplayHandle,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    write: &mut W,
) -> Result<u64, PeerError> {
    let mut written = None;
    let mut boundary_done = false;
    let mut replayed_frames = 0u64;
    let mut phase = crate::timing::start();
    // Of the phase, the time spent writing batches to the connection.
    let mut writing = std::time::Duration::ZERO;
    loop {
        write_addressed(run.catchup, peer, session, write).await?;
        let authority = run.authority.clone();
        let source = StateSource::new(peer, session);
        let preparing = async {
            authorize_replay(run.workspace, &source, &authority).await?;
            handle.advance(&authority, written.take()).await
        };
        match preparing.await? {
            ReplayAdvance::Preparing => {}
            ReplayAdvance::Batch(frames) => {
                let batch_at = crate::timing::start();
                written = Some(write_replay_batch(run, frames, peer, session, write).await?);
                if let Some(batch_at) = batch_at {
                    writing = writing.saturating_add(batch_at.elapsed());
                }
                continue;
            }
            ReplayAdvance::Exhausted(summary) => {
                replayed_frames = replayed_frames.saturating_add(summary.total_frames);
                session.metrics.add_replayed_pending(summary.pending_frames);
                session.metrics.add_replayed_retained(
                    summary.total_frames.saturating_sub(summary.pending_frames),
                );
                if boundary_done {
                    crate::timing::event("join.catchup", phase, summary.total_frames);
                    run.catchup.discard_held(run.owner.workers()).await;
                    // A catch-up that replayed nothing from the device's own
                    // queue has nothing to unlink, and needs no owner turn.
                    if summary.cleanup_entries == 0 {
                        crate::timing::window("join.cleanup_skipped", crate::timing::start());
                    } else {
                        let cleaning = crate::timing::start();
                        finish_replay_cleanup(run, &handle, peer, session).await?;
                        crate::timing::event("join.cleanup", cleaning, summary.cleanup_entries);
                    }
                    return Ok(replayed_frames);
                }
                crate::timing::event("join.replay", phase, summary.total_frames);
                crate::timing::event(
                    "join.replay_write",
                    phase.and_then(|_| std::time::Instant::now().checked_sub(writing)),
                    summary.total_frames,
                );
                let cutting = crate::timing::start();
                let Some(catchup_handle) = cut_replay_boundary(run, &handle, peer, session).await?
                else {
                    crate::timing::event("join.boundary", cutting, 0);
                    return Ok(replayed_frames);
                };
                crate::timing::event("join.boundary", cutting, 1);
                phase = crate::timing::start();
                handle = catchup_handle;
                written = None;
                boundary_done = true;
                continue;
            }
        }
        written = None;
    }
}

/// Write one replay batch to the wire and record each frame in the
/// subscriber's resume log.
async fn write_replay_batch<W: tokio::io::AsyncWrite + Unpin>(
    run: ReplayRun<'_>,
    mut frames: crate::store::replay::ReplayBatch,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    write: &mut W,
) -> Result<WrittenBatch, PeerError> {
    // A batch is authorised once, as presence authorises each
    // workspace's run: checked per frame, a crowd's replays queued
    // every writer in the workspace on its lock for every frame.
    let mut workspaces: Vec<(u64, bool)> = Vec::new();
    for workspace in frames.frames().filter_map(state_frame_room) {
        if !workspaces.contains(&workspace) {
            workspaces.push(workspace);
        }
    }
    for (id, subscribed) in workspaces {
        authorize_state_recipient(id, subscribed, peer, session).await?;
    }
    authorize_replay(
        run.workspace,
        &StateSource::new(peer, session),
        run.authority,
    )
    .await?;
    while let Some(frame) = frames.front() {
        write
            .write_all(&encode_charged(frame, session, false)?)
            .await?;
        trace_native_publication("replay_written", frame, Some(&peer.device_public_key));
        // The position follows after `Ready`; a reconnect
        // before it replays these frames once more.
        let _ = resume_record(session, run.workspace_id, [frame]);
        frames.pop_written();
    }
    let written = WrittenBatch(frames.number);
    // No frame or output permit escapes this one outstanding batch.
    drop(frames);
    Ok(written)
}

/// Clean up a catch-up replay once it is written, in one turn of the owner.
async fn finish_replay_cleanup(
    run: ReplayRun<'_>,
    handle: &ReplayHandle,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<(), PeerError> {
    let authority = run.authority.clone();
    let source = StateSource::new(peer, session);
    let workspace = run.workspace.clone();
    let checking_workspace = run.workspace.clone();
    let checking_source = source.clone();
    let cleanup_handle = handle.clone();
    let cleanup = run.owner.submit_replay_resumable_wait(
        1,
        async move {
            authorize_replay(&workspace, &source, &authority).await?;
            Ok(authority)
        },
        move |state, authority| {
            let authority = authority.as_ref().invariant("replay cleanup")?;
            crate::timing::windowed("owner.replay.cleanup_step", || {
                state.finish_replay(authority, &cleanup_handle, |store| {
                    authorize_replay_now(store, &checking_workspace, &checking_source, authority)
                })
            })
        },
        |result| async { result },
    );
    let _ = Box::pin(replay_wait(cleanup)).await?;
    Ok(())
}

/// Clean up the first replay and cut what committed while it ran. Returns
/// the handle of that catch-up replay, or `None` when nothing committed.
async fn cut_replay_boundary(
    run: ReplayRun<'_>,
    handle: &ReplayHandle,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<ReplayHandle>, PeerError> {
    let authority = run.authority.clone();
    let source = StateSource::new(peer, session);
    let workspace = run.workspace.clone();
    let checking_workspace = run.workspace.clone();
    let checking_source = source.clone();
    let cleanup_handle = handle.clone();
    // The first replay's cleanup and the cut of what
    // committed while it ran are one turn of the owner:
    // two, a newcomer waited behind every queued commit
    // twice before it was caught up.
    let checking = run.catchup.clone();
    let completing = run.catchup.clone();
    let boundary_workspace = run.workspace.clone();
    let boundary_source = source.clone();
    let boundary_authority = authority.clone();
    let mut finished = false;
    let boundary = run.owner.submit_replay_resumable_wait(
        1,
        async move {
            authorize_replay(&workspace, &source, &authority).await?;
            Ok(authority)
        },
        move |state, authority| {
            let authority = authority.as_ref().invariant("replay boundary")?;
            if !finished {
                let step = crate::timing::windowed("owner.replay.boundary_cleanup_step", || {
                    state.finish_replay(authority, &cleanup_handle, |store| {
                        authorize_replay_now(
                            store,
                            &checking_workspace,
                            &checking_source,
                            authority,
                        )
                    })
                })?;
                if matches!(step, Step::More) {
                    return Ok(Step::More);
                }
                finished = true;
            }
            if !checking.dirty.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(Step::Done(None));
            }
            let step = crate::timing::windowed("owner.replay.boundary_capture_step", || {
                state.capture_pending_replay(authority, &checking.held)
            })?;
            match step {
                Step::More => Ok(Step::More),
                Step::Done(handle) => Ok(Step::Done(Some(handle))),
            }
        },
        move |result| async move {
            let result = result?;
            finish_replay_boundary(
                &boundary_workspace,
                &boundary_source,
                &boundary_authority,
                &completing,
            )
            .await?;
            Ok(result)
        },
    );
    Box::pin(replay_wait(boundary)).await
}

pub(super) async fn authorize_replay(
    workspace: &SharedWorkspace,
    source: &StateSource,
    authority: &crate::persistence::ReplayAuthority,
) -> Result<(), StoreError> {
    let runtime = crate::workspace_io::fresh_membership(workspace).await?;
    if !source.authorized(&runtime)
        || !runtime
            .state_sessions
            .get(&source.peer.device_public_key)
            .is_some_and(|session| authority.matches(&session.live))
    {
        return Err(crate::persistence::error(
            "replay subscriber authorization changed",
        ));
    }
    Ok(())
}

async fn finish_replay_boundary(
    workspace: &SharedWorkspace,
    source: &StateSource,
    authority: &crate::persistence::ReplayAuthority,
    catchup: &Arc<ReplayCatchup>,
) -> Result<(), StoreError> {
    authorize_replay(workspace, source, authority).await?;
    let runtime = workspace.lock().await;
    let current = runtime
        .state_sessions
        .get(&source.peer.device_public_key)
        .and_then(|session| session.replay.as_ref())
        .filter(|current| Arc::ptr_eq(current, catchup))
        .ok_or_else(|| crate::persistence::error("replay session boundary changed"))?;
    current
        .phase
        .store(ReplayCatchup::BOUNDARY, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the session is read from the membership guard held for the whole boundary check"
)]
async fn authorize_replay_boundary(
    workspace: &SharedWorkspace,
    source: &StateSource,
    authority: &crate::persistence::ReplayAuthority,
    catchup: &Arc<ReplayCatchup>,
) -> Result<(), StoreError> {
    authorize_replay(workspace, source, authority).await?;
    let runtime = workspace.lock().await;
    let session = runtime
        .state_sessions
        .get(&source.peer.device_public_key)
        .filter(|session| authority.matches(&session.live))
        .filter(|session| {
            session
                .replay
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, catchup))
        })
        .ok_or_else(|| crate::persistence::error("replay session boundary changed"))?;
    if session
        .replay
        .as_ref()
        .invariant("replay missing")?
        .phase
        .load(std::sync::atomic::Ordering::SeqCst)
        != ReplayCatchup::BOUNDARY
    {
        return Err(crate::persistence::error(
            "replay session boundary not ready",
        ));
    }
    Ok(())
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the session is read from the membership guard held until readiness is recorded"
)]
async fn complete_replay_ready(
    workspace: &SharedWorkspace,
    source: &StateSource,
    authority: &crate::persistence::ReplayAuthority,
    catchup: &Arc<ReplayCatchup>,
) -> Result<(), StoreError> {
    let runtime = workspace.lock().await;
    let session = runtime
        .state_sessions
        .get(&source.peer.device_public_key)
        .filter(|session| authority.matches(&session.live))
        .filter(|session| {
            session
                .replay
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, catchup))
        })
        .ok_or_else(|| crate::persistence::error("replay session fenced during Ready"))?;
    let replay = session.replay.as_ref().invariant("replay missing")?;
    if replay
        .phase
        .compare_exchange(
            ReplayCatchup::BOUNDARY,
            ReplayCatchup::READY,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_err()
    {
        return Err(crate::persistence::error(
            "replay session boundary changed during Ready",
        ));
    }
    Ok(())
}
/// Blocking-worker authority check, immediately before one exact cleanup entry.
/// Disk I/O precedes the short routing check; no routing guard crosses I/O.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the runtime guard is read by every membership condition below; clippy misreads its last use"
)]
fn authorize_replay_now(
    store: &crate::store::WorkspaceStore,
    workspace: &SharedWorkspace,
    source: &StateSource,
    authority: &crate::persistence::ReplayAuthority,
) -> Result<(), StoreError> {
    // The in-memory set while the membership watch vouches for it; otherwise
    // the file, as a fresh membership reload would read it.
    let locking = crate::timing::start();
    let current = workspace.blocking_lock().membership_is_current();
    crate::timing::window("replay.authorize_lock", locking);
    let members = if current {
        None
    } else {
        let loading = crate::timing::start();
        let members = store.load_membership()?;
        crate::timing::window("replay.authorize_membership_load", loading);
        Some(members)
    };
    let locking = crate::timing::start();
    let runtime = workspace.blocking_lock();
    crate::timing::window("replay.authorize_lock", locking);
    let workspace = members.as_deref().map_or_else(
        || workspace_access::WorkspaceFacts::current(&runtime),
        |listed| workspace_access::WorkspaceFacts::with_listed(&runtime, listed),
    );
    let member = workspace_access::decide_in(
        workspace,
        &source.trust(),
        &source.peer,
        WorkspaceOperation::Replay,
        unix_now_ns(),
    )
    .admitted();
    if !member
        || !source.authorized(&runtime)
        || !runtime
            .state_sessions
            .get(&source.peer.device_public_key)
            .is_some_and(|session| authority.matches(&session.live))
    {
        return Err(crate::persistence::error(
            "replay cleanup authority changed",
        ));
    }
    Ok(())
}
/// Wait for a durable replay step to complete.
async fn replay_wait<T>(
    admission: impl std::future::Future<
        Output = Result<tokio::sync::oneshot::Receiver<Result<T, StoreError>>, StoreError>,
    >,
) -> Result<T, PeerError> {
    let receipt = admission.await?;
    Ok(crate::persistence::receipt(receipt).await?)
}

pub(super) struct ReplayCatchup {
    pub(super) phase: std::sync::atomic::AtomicU8,
    pub(super) dirty: std::sync::atomic::AtomicBool,
    /// Where what the workspace commits while this replay runs waits for
    /// the replay's final cut. It is this session's alone and the commits
    /// are already durable in retained history, so nothing here is synced:
    /// a crash ends the session, and its next one replays retained history.
    /// The store's work reclamation discards what a crash leaves.
    held: std::path::PathBuf,
    /// Frames committed while this replay runs that are addressed to this
    /// device alone among the workspace's (the Welcome that admits it to a
    /// channel's group, [`neighbour_wire::audience`]): written between the
    /// replay's batches rather than after its last one. Nothing the replay
    /// carries is what such a frame builds on, and the keys it brings open
    /// what the replay is still sending, so a page shows a record while the
    /// rest of the workspace arrives. The final cut carries them again, which
    /// the device ignores as a frame it already applied.
    addressed: std::sync::Mutex<Vec<Arc<Envelope>>>,
}
impl ReplayCatchup {
    pub(super) const ACTIVE: u8 = 0;
    pub(super) const BOUNDARY: u8 = 1;
    const READY: u8 = 2;

    /// A catch-up whose held commits go under `work`, the store's work
    /// directory.
    pub(super) fn new(work: &std::path::Path) -> Self {
        Self {
            phase: std::sync::atomic::AtomicU8::new(Self::ACTIVE),
            dirty: std::sync::atomic::AtomicBool::new(false),
            held: work.join(crate::store::replay::held_work_name(rand::random())),
            addressed: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Hand `frame`, addressed to this device, to the replay's next pause
    /// between batches. Past [`MAX_ADDRESSED`] waiting frames the newest is
    /// left to the final cut, which carries every one of them anyway.
    pub(super) fn address(&self, frame: Arc<Envelope>) {
        let mut addressed = self.addressed.lock_recover();
        if addressed.len() < MAX_ADDRESSED {
            addressed.push(frame);
        }
    }

    /// The frames addressed to this device since the last call, oldest first.
    fn take_addressed(&self) -> Vec<Arc<Envelope>> {
        std::mem::take(&mut *self.addressed.lock_recover())
    }

    /// Hold `bytes`, the commit at host order `sequence`, for the final cut.
    pub(super) fn hold(&self, sequence: u64, bytes: &[u8]) -> Result<(), crate::store::StoreError> {
        std::fs::create_dir_all(&self.held)?;
        // Named as the device's queue names its entries, so the cut takes
        // them in host order.
        let name = format!("{sequence:016x}-{}.delta", blake3::hash(bytes).to_hex());
        std::fs::write(self.held.join(name), bytes)?;
        Ok(())
    }

    /// Remove the commits this catch-up held, once its final cut is written:
    /// the cut pinned what it replays, and nothing is held past the cut (a
    /// commit is held only while the catch-up is [`Self::ACTIVE`]). What
    /// cannot be removed now is left to the drop and to work reclamation.
    async fn discard_held(&self, workers: Arc<tokio::sync::Semaphore>) {
        let held = self.held.clone();
        let removed =
            crate::persistence::blocking(workers, move || match std::fs::remove_dir_all(held) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                removed => removed.map_err(StoreError::from),
            })
            .await
            .and_then(|removed| removed);
        if let Err(error) = removed {
            tracing::debug!(%error, "a written catch-up's held commits were left for reclamation");
        }
    }
}
impl Drop for ReplayCatchup {
    fn drop(&mut self) {
        match std::fs::remove_dir_all(&self.held) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::debug!(%error, "a finished catch-up's held commits were left for reclamation");
            }
        }
    }
}
