//! Replay writing: resume logs, the replay catch-up a subscriber's backlog
//! runs under, writing replay batches and boundaries to the wire, and
//! authorizing each state frame before it is written.

use super::{
    Arc, AsyncWriteExt, AuthenticatedPeer, Envelope, EnvelopeV2, Invariant, PeerError,
    PrivateOutbound, ReplayAdvance, ReplayHandle, ReplayTicket, RoomOperation, SessionCtx,
    SharedWorkspace, StateSource, Step, StoreError, WrittenBatch, current_session, encode_charged,
    mpsc, room_access, test_point, trace_native_publication, unix_now_ns, write_private_delivery,
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
/// Queue a reply to the peer's own private request. A full queue means the
/// peer is not reading its replies yet: wait for the writer, which stops this
/// reader and so backpressures the peer, rather than dropping the connection.
pub(super) async fn send_private_reply(
    private_tx: &mpsc::Sender<PrivateOutbound>,
    reply: EnvelopeV2,
) -> Result<(), PeerError> {
    private_tx
        .send(PrivateOutbound::Reply(reply))
        .await
        .map_err(|_| PeerError::Protocol("private reply writer closed".into()))
}

pub(super) async fn write_private<W: tokio::io::AsyncWrite + Unpin>(
    private: PrivateOutbound,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    write: &mut W,
) -> Result<(), PeerError> {
    match private {
        PrivateOutbound::Reply(reply) => {
            write
                .write_all(&tilo_delivery_protocol::transport_v2::encode(&reply)?)
                .await?;
        }
        PrivateOutbound::Delivery(delivery) => {
            write_private_delivery(&delivery, peer, session, write).await?;
        }
    }
    Ok(())
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

/// The room a state frame goes to, and whether it needs this connection to
/// be the room's current subscriber; `None` for frames that need neither.
const fn state_frame_room(frame: &Envelope) -> Option<(u64, bool)> {
    match frame {
        Envelope::Delta { workspace_id, .. }
        | Envelope::Ready { workspace_id, .. }
        | Envelope::Presence { workspace_id, .. } => Some((*workspace_id, true)),
        Envelope::SyncResponse { workspace_id, .. } => Some((*workspace_id, false)),
        _ => None,
    }
}

/// [`authorize_state_frame`] for a frame to room `id`; `subscribed` frames
/// (deltas, presence, `Ready`) also need this connection to be the room's
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
    if !session.admits(&wr, peer, RoomOperation::State)
        || ((subscribed || wr.subscribers.contains_key(&peer.device_public_key))
            && !current_session(&wr, peer, session.outbound))
    {
        return Err(PeerError::Protocol(
            "state recipient authorization changed".into(),
        ));
    }
    Ok(())
}
/// One subscription's replay as [`finish_replay_write`] drives it: the room,
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
    private_rx: &mut mpsc::Receiver<PrivateOutbound>,
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
        let replaying = crate::timing::start();
        let replayed_frames = drive_replay(run, handle, peer, session, private_rx, write).await?;
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
            test_point!(pause(room workspace, "replay-before-ready")
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
    private_rx: &mut mpsc::Receiver<PrivateOutbound>,
    write: &mut W,
) -> Result<u64, PeerError> {
    let mut written = None;
    let mut boundary_done = false;
    let mut replayed_frames = 0u64;
    let mut phase = crate::timing::start();
    // Of the phase, the time spent writing batches to the connection.
    let mut writing = std::time::Duration::ZERO;
    loop {
        let authority = run.authority.clone();
        let source = StateSource::new(peer, session);
        let preparing = async {
            authorize_replay(run.workspace, &source, &authority).await?;
            handle.advance(&authority, written.take()).await
        };
        match replay_direct_wait(preparing, peer, session, private_rx, write).await? {
            ReplayAdvance::Preparing => {}
            ReplayAdvance::Batch(frames) => {
                let batch_at = crate::timing::start();
                written =
                    Some(write_replay_batch(run, frames, peer, session, private_rx, write).await?);
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
                    let cleaning = crate::timing::start();
                    finish_replay_cleanup(run, &handle, peer, session, private_rx, write).await?;
                    crate::timing::event("join.cleanup", cleaning, 0);
                    return Ok(replayed_frames);
                }
                crate::timing::event("join.replay", phase, summary.total_frames);
                crate::timing::event(
                    "join.replay_write",
                    phase.and_then(|_| std::time::Instant::now().checked_sub(writing)),
                    summary.total_frames,
                );
                let cutting = crate::timing::start();
                let Some(catchup_handle) =
                    cut_replay_boundary(run, &handle, peer, session, private_rx, write).await?
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

/// Write one replay batch to the wire, letting queued private replies
/// overtake it, and record each frame in the subscriber's resume log.
async fn write_replay_batch<W: tokio::io::AsyncWrite + Unpin>(
    run: ReplayRun<'_>,
    mut frames: crate::store::replay::ReplayBatch,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    private_rx: &mut mpsc::Receiver<PrivateOutbound>,
    write: &mut W,
) -> Result<WrittenBatch, PeerError> {
    // A batch is authorised once, as presence authorises each
    // room's run: checked per frame, a crowd's replays queued
    // every writer in the room on its lock for every frame.
    let mut rooms: Vec<(u64, bool)> = Vec::new();
    for room in frames.frames().filter_map(state_frame_room) {
        if !rooms.contains(&room) {
            rooms.push(room);
        }
    }
    for (id, subscribed) in rooms {
        authorize_state_recipient(id, subscribed, peer, session).await?;
    }
    authorize_replay(
        run.workspace,
        &StateSource::new(peer, session),
        run.authority,
    )
    .await?;
    while let Some(frame) = frames.front() {
        for _ in 0..8 {
            let Ok(private) = private_rx.try_recv() else {
                break;
            };
            write_private(private, peer, session, write).await?;
        }
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
async fn finish_replay_cleanup<W: tokio::io::AsyncWrite + Unpin>(
    run: ReplayRun<'_>,
    handle: &ReplayHandle,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    private_rx: &mut mpsc::Receiver<PrivateOutbound>,
    write: &mut W,
) -> Result<(), PeerError> {
    let authority = run.authority.clone();
    let source = StateSource::new(peer, session);
    let workspace = run.workspace.clone();
    let checking_workspace = run.workspace.clone();
    let checking_source = source.clone();
    let cleanup_handle = handle.clone();
    let cleanup = run.owner.submit_replay_resumable_wait(
        false,
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
    let _ = Box::pin(replay_wait(cleanup, peer, session, private_rx, write)).await?;
    Ok(())
}

/// Clean up the first replay and cut what committed while it ran. Returns
/// the handle of that catch-up replay, or `None` when nothing committed.
async fn cut_replay_boundary<W: tokio::io::AsyncWrite + Unpin>(
    run: ReplayRun<'_>,
    handle: &ReplayHandle,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    private_rx: &mut mpsc::Receiver<PrivateOutbound>,
    write: &mut W,
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
        true,
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
    Box::pin(replay_wait(boundary, peer, session, private_rx, write)).await
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
    let room = members.as_deref().map_or_else(
        || room_access::Room::current(&runtime),
        |listed| room_access::Room::with_listed(&runtime, listed),
    );
    let member = room_access::decide_in(
        room,
        &source.trust(),
        &source.peer,
        RoomOperation::Replay,
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
async fn replay_wait<T, W: tokio::io::AsyncWrite + Unpin>(
    admission: impl std::future::Future<
        Output = Result<tokio::sync::oneshot::Receiver<Result<T, StoreError>>, StoreError>,
    >,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    private_rx: &mut mpsc::Receiver<PrivateOutbound>,
    write: &mut W,
) -> Result<T, PeerError> {
    // Both bounded admission and durable completion must leave the private
    // writer live; awaiting admission before this select would stall replies.
    let completed = async {
        let receipt = admission.await?;
        crate::persistence::receipt(receipt).await
    };
    replay_direct_wait(completed, peer, session, private_rx, write).await
}

pub(super) async fn replay_direct_wait<T, W: tokio::io::AsyncWrite + Unpin>(
    operation: impl std::future::Future<Output = Result<T, StoreError>>,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    private_rx: &mut mpsc::Receiver<PrivateOutbound>,
    write: &mut W,
) -> Result<T, PeerError> {
    tokio::pin!(operation);
    let mut private_open = session.framing.is_v2();
    loop {
        tokio::select! {
            biased;
            result = &mut operation => return Ok(result?),
            private = private_rx.recv(), if private_open => {
                match private {
                    Some(private) => {
                        let writing = write_private(private, peer, session, write);
                        tokio::pin!(writing);
                        let mut completed = None;
                        loop {
                            tokio::select! {
                                biased;
                                result = &mut writing => {
                                    result?;
                                    if let Some(result) = completed {
                                        return Ok(result?);
                                    }
                                    break;
                                }
                                result = &mut operation, if completed.is_none() => {
                                    // Finish the active private wire frame before
                                    // propagating either replay success or failure.
                                    completed = Some(result);
                                }
                            }
                        }
                    }
                    None => private_open = false,
                }
            }
        }
    }
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
        }
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
