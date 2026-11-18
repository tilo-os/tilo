//! `Subscribe`, `SubscribeFrom` and `Unsubscribe`: admission, resume logs,
//! replay capture and live-subscriber registration.

use super::{
    Arc, AuthenticatedPeer, Decision, Envelope, Lag, PeerError, ReplayCatchup, ReplayHandle,
    ReplayTicket, ResumeLog, SessionCtx, SharedWorkspace, StateDelivery, StateSession, StateSource,
    Step, StoreError, Subscriber, WorkspaceOperation, current_session, mpsc,
    session_peer_authorized, unix_now_ns,
};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::WorkspaceId;

/// How a subscription asks for its replay.
pub(super) enum Subscription {
    /// `Subscribe`: everything, with no positions.
    Whole,
    /// `SubscribeFrom`: what the device has not received since the position.
    From(Option<Vec<u8>>),
}

/// Set up this subscription's resume log. A plain `Subscribe` has none. A
/// `SubscribeFrom` with a position this host issued to this device for this
/// store continues that log and returns the retained frames the device holds,
/// which its replay leaves out; any other `SubscribeFrom` starts a new log
/// and replays everything.
async fn open_resume(
    session: &SessionCtx<'_>,
    peer: &AuthenticatedPeer,
    workspace_id: WorkspaceId,
    store: crate::store::WorkspaceStore,
    resume: Subscription,
) -> Result<Option<crate::store::replay::ResumeSkip>, PeerError> {
    let (Subscription::From(position), Some(issuer)) = (resume, session.positions) else {
        session.resumes.lock_recover().remove(&workspace_id);
        return Ok(None);
    };
    let device = peer.device_public_key;
    let opened = tokio::task::spawn_blocking(move || {
        let generation = store.generation()?;
        let held = match position
            .as_deref()
            .and_then(|position| issuer.verify(position, workspace_id, &device, &generation))
        {
            Some(resume) => match store.load_resume(&device, &resume.log, resume.len)? {
                Some(held) => store
                    .fork_resume(&device, &resume.log, resume.len)?
                    .map(|log| {
                        (
                            crate::position::Resume {
                                log,
                                len: resume.len,
                            },
                            held,
                        )
                    }),
                None => None,
            },
            None => None,
        };
        let (resume, skip) = match held {
            Some((resume, held)) => (resume, Some(Arc::new(held))),
            None => (
                crate::position::Resume {
                    log: store.start_resume(&device)?,
                    len: 0,
                },
                None,
            ),
        };
        Ok::<_, crate::store::StoreError>((
            ResumeLog {
                store,
                issuer,
                device,
                generation,
                resume,
            },
            skip,
        ))
    })
    .await
    .map_err(|error| PeerError::Io(std::io::Error::other(error)))??;
    let (log, skip) = opened;
    session.resumes.lock_recover().insert(workspace_id, log);
    Ok(skip)
}

/// A workspace that admitted a `Subscribe`, with what its admitting membership
/// read showed.
struct AdmittedSubscription {
    workspace: SharedWorkspace,
    store: crate::store::WorkspaceStore,
}

/// How [`admit_subscription`] answered a `Subscribe`.
enum SubscribeAdmission {
    Admitted(AdmittedSubscription),
    /// Refused without ending the session; the reply to send.
    Refused(Envelope),
}

/// A subscription's replay, ready for the writer: the workspace, its ticket,
/// its captured replay and what commits while it is written.
pub(super) type ReplaySlot = (WorkspaceId, ReplayTicket, ReplayHandle, Arc<ReplayCatchup>);

/// How [`subscribe`] answered a `Subscribe`.
pub(super) enum Subscribed {
    /// Admitted and captured: the replay to write, then `Ready`.
    Replaying(ReplaySlot),
    /// Refused without ending the session; the reply to send.
    Refused(Envelope),
}

/// Handle `Subscribe`: check on membership, replay retained baselines + this
/// device's pending queue, then register the live subscriber.
pub(super) async fn handle_subscribe(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<WorkspaceId>,
    replay: &mut Option<ReplaySlot>,
    workspace_id: WorkspaceId,
    resume: Subscription,
) -> Result<Option<Envelope>, PeerError> {
    let subscribed_now = subscribe(peer, session, subscribed, workspace_id, resume).await?;
    match subscribed_now {
        Subscribed::Refused(refusal) => Ok(Some(refusal)),
        Subscribed::Replaying(slot) => {
            *replay = Some(slot);
            if !subscribed.contains(&workspace_id) {
                subscribed.push(workspace_id);
            }
            Ok(None)
        }
    }
}

/// The work of a `Subscribe` for `workspace_id`, which reads the
/// connection's subscriptions but leaves adding it to them to the caller:
/// subscriptions to different workspaces run side by side
/// ([`super::session`]'s subscribe runs).
#[expect(
    clippy::significant_drop_tightening,
    reason = "the ticket and replay handle move into the returned replay slot; the lint does not see the move"
)]
pub(super) async fn subscribe(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    workspace_id: WorkspaceId,
    resume: Subscription,
) -> Result<Subscribed, PeerError> {
    let AdmittedSubscription {
        workspace: ws,
        store,
    } = {
        let admission = admit_subscription(peer, session, subscribed, workspace_id).await?;
        match admission {
            SubscribeAdmission::Admitted(admitted) => admitted,
            SubscribeAdmission::Refused(refusal) => return Ok(Subscribed::Refused(refusal)),
        }
    };
    super::test_hooks::test_point!(signal(store.root(), "subscribe-admitted"));
    let work = store.root().join(crate::store::replay::WORK_DIR);
    let skip = open_resume(session, peer, workspace_id, store, resume).await?;
    let owner = crate::persistence::handle(&ws).await;
    let source = StateSource::new(peer, session);
    let state_tx = session
        .state_outbound
        .cloned()
        .unwrap_or_else(|| mpsc::channel(1).0);
    let completion_workspace = ws.clone();
    let waited = crate::timing::start();
    let mut ticket =
        ReplayTicket::reserve(peer.device_public_key, session.connection_live.cloned()).await?;
    // A host this workspace is attached to keeps all of the workspace; a device
    // opens on-demand records. A browser environment's device is replayed its
    // own record and nothing else of the workspace.
    if ws.lock().await.attached(&peer.device_public_key) {
        ticket = ticket.with_every_document();
    } else if let Some(scope) = session.environment(&peer.device_public_key) {
        let channel = scope.channel;
        ticket = ticket.with_channels(std::iter::once(channel).collect());
    }
    crate::timing::event("join.ticket_wait", waited, 0);
    let capturing = crate::timing::start();
    let capture = ticket.authority.clone();
    let bind = ticket.authority.clone();
    let catchup = Arc::new(ReplayCatchup::new(&work));
    // What is committed for a device the entry just admitted (its channel
    // Welcome) goes ahead of everything its replay carries.
    ws.lock()
        .await
        .arriving
        .arrived(&peer.device_public_key, &catchup);
    let registration = SubscriberRegistration {
        workspace: completion_workspace,
        source,
        bind,
        presence: session.presence.cloned(),
        state_tx,
        catchup: catchup.clone(),
        lag: session.lag.clone(),
        content: Arc::clone(session.content),
        channel_reader: session
            .channel_reader
            .load(std::sync::atomic::Ordering::Relaxed),
    };

    let rx = owner
        .submit_replay_resumable_wait(
            1,
            async { Ok(()) },
            move |state, _| {
                let step = crate::timing::windowed("owner.replay.capture_step", || {
                    state.capture_resumed_replay(&capture, skip.clone())
                })?;
                match step {
                    Step::More => Ok(Step::More),
                    Step::Done(handle) => Ok(Step::Done(handle)),
                }
            },
            move |result| async move { register_subscriber(registration, result?).await },
        )
        .await?;
    let completion = crate::persistence::receipt(rx).await?;
    crate::timing::event("join.capture", capturing, 0);
    let handle = completion;
    // Captured: whatever the replay waits on from here is its own page's
    // network, which no other page's replay may wait behind.
    ticket.authority.release_slot();
    session.metrics.inc_subscribes();
    Ok(Subscribed::Replaying((
        workspace_id,
        ticket,
        handle,
        catchup,
    )))
}

/// The admission half of [`handle_subscribe`]: the agent's delegation, the
/// workspace this host holds, and the workspace's decision under a fresh
/// membership read, enrolling the device when the workspace says so. A device
/// the server does not admit is refused as not admitted; a connection whose
/// own authorization changed meanwhile ends.
async fn admit_subscription(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    workspace_id: WorkspaceId,
) -> Result<SubscribeAdmission, PeerError> {
    // A workspace its owner deleted here is gone for good: whoever asks is
    // told so, and forgets it rather than tries again.
    if session
        .admin
        .is_some_and(|workspaces| workspaces.deleted(workspace_id))
    {
        return Ok(SubscribeAdmission::Refused(Envelope::NotAuthorised {
            workspace_id,
            refusal: tilo_server_protocol::refusal::Refusal::WorkspaceDeleted,
        }));
    }
    // An agent outside its delegation never gets a workspace bound for it.
    if let Err(reason) = session
        .trust()
        .agent(&peer.device_public_key, workspace_id, unix_now_ns())
    {
        return Ok(SubscribeAdmission::Refused(Envelope::NotAuthorised {
            workspace_id,
            refusal: reason.refusal(),
        }));
    }
    // Only a workspace this host holds admits a device here: a person's own
    // server makes theirs, and one made elsewhere is handed over.
    let ws = session.workspaces.lock().await.get(&workspace_id).cloned();
    let Some(ws) = ws else {
        // A host that keeps nothing of this person forgot them: their session
        // stops as not admitted, and they join again through its entry. One
        // that knows them holds no such workspace yet: it is not ready until
        // the person's device hands it over or their server makes it.
        let known = session
            .admin
            .is_some_and(|workspaces| workspaces.knows(&peer.user_public_key));
        return Ok(SubscribeAdmission::Refused(Envelope::NotAuthorised {
            workspace_id,
            refusal: if known {
                tilo_server_protocol::refusal::Refusal::NotReady
            } else {
                tilo_server_protocol::refusal::Refusal::NotAdmitted
            },
        }));
    };
    let mut wr = crate::workspace_io::fresh_membership(&ws).await?;
    // The certificate passed when this session first subscribed: failing now
    // is a change mid-session. A first subscribe that fails it is refused
    // below, with no workspace data.
    if !subscribed.is_empty() && !session_peer_authorized(peer, session) {
        return Err(PeerError::Protocol(
            "subscriber authorization changed".into(),
        ));
    }
    if subscribed.contains(&workspace_id) && !current_session(&wr, peer, session.outbound) {
        return Err(PeerError::Protocol(
            "subscriber authorization changed".into(),
        ));
    }
    // A host attached to this one holds its session under an active lease,
    // so it doesn't need to appear in the workspace membership list.
    match session.decide(&wr, peer, WorkspaceOperation::Subscribe) {
        Decision::Admit => {}
        Decision::AdmitAndEnrol => {
            let revision = wr.membership_revision;
            drop(wr);
            crate::workspace_io::enrol(&ws, &peer.cert, revision).await?;
            wr = crate::workspace_io::fresh_membership(&ws).await?;
            if !session.admits(&wr, peer, WorkspaceOperation::Subscribe) {
                return Err(PeerError::Protocol(
                    "subscriber authorization changed".into(),
                ));
            }
            if subscribed.contains(&workspace_id) && !current_session(&wr, peer, session.outbound) {
                return Err(PeerError::Protocol(
                    "subscriber authorization changed".into(),
                ));
            }
        }
        Decision::Deny(reason) => {
            return Ok(SubscribeAdmission::Refused(Envelope::NotAuthorised {
                workspace_id,
                refusal: reason.refusal(),
            }));
        }
    }
    let store = wr.store.clone();
    drop(wr);
    Ok(SubscribeAdmission::Admitted(AdmittedSubscription {
        workspace: ws,
        store,
    }))
}

/// What a subscriber's replay completion registers in the workspace once its
/// capture is ready.
struct SubscriberRegistration {
    workspace: SharedWorkspace,
    source: StateSource,
    /// The replay ticket's authority, bound to the session's liveness.
    bind: Arc<crate::persistence::ReplayAuthority>,
    presence: Option<Arc<crate::presence_mailbox::PresenceMailbox>>,
    state_tx: mpsc::Sender<StateDelivery>,
    catchup: Arc<ReplayCatchup>,
    lag: Arc<Lag>,
    /// The connection's queue for content its outbound channel could not
    /// take yet ([`crate::content_queue`]).
    content: Arc<crate::content_queue::ContentQueue>,
    channel_reader: bool,
}

/// The completion half of [`handle_subscribe`], run under the workspace's lock
/// once the replay capture is ready: re-check the subscriber against the
/// workspace, then bind the ticket and register the live subscriber, its
/// presence route and its state session.
async fn register_subscriber(
    registration: SubscriberRegistration,
    handle: ReplayHandle,
) -> Result<ReplayHandle, StoreError> {
    let SubscriberRegistration {
        workspace: completion_workspace,
        source,
        bind,
        presence,
        state_tx,
        catchup: session_catchup,
        lag: session_lag,
        content,
        channel_reader,
    } = registration;
    let mut wr = crate::workspace_io::fresh_membership(&completion_workspace).await?;
    if !source.alive() || !source.admits(&wr, &source.peer) {
        return Err(crate::persistence::error(
            "replay subscriber authorization changed",
        ));
    }
    let device = source.peer.device_public_key;
    // The connection subscribing again keeps its state stream's liveness.
    let live = wr
        .session(&device, &source.tx)
        .and_then(|current| current.state.as_ref())
        .map_or_else(
            || Arc::new(std::sync::atomic::AtomicBool::new(true)),
            |state| state.live.clone(),
        );
    // Binding may fail when the waiting connection/receipt was dropped.
    // It must precede every registration side effect.
    bind.bind(live.clone())?;
    let tx = source.tx.clone();
    wr.add_subscriber(Subscriber {
        device_public_key: device,
        tx: source.tx,
        disconnect: source.disconnect,
        content_waiting: content,
    });
    if let Some(mailbox) = presence {
        wr.route_presence(device, &tx, mailbox);
    }
    // A device is its person's for good: what a write to that person's own
    // record is queued for while the device is away.
    wr.store
        .remember_person(device, source.peer.user_public_key);
    wr.set_state_session(
        &device,
        &tx,
        StateSession {
            live,
            peer: source.peer,
            tx: state_tx,
            replay: Some(session_catchup),
            lag: Some(session_lag),
            channel_reader,
        },
    );
    drop(wr);
    Ok(handle)
}

/// Handle `Unsubscribe`: drop the live subscriber (if the workspace
/// is known) and stop tracking it for this connection's teardown.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the flagged value is a shared workspace handle (an Arc clone); dropping it earlier releases no lock"
)]
pub(super) async fn handle_unsubscribe(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<WorkspaceId>,
    workspace_id: WorkspaceId,
) -> Result<Option<Envelope>, PeerError> {
    let ws = {
        let map = session.workspaces.lock().await;
        map.get(&workspace_id).cloned()
    };
    if let Some(ws) = ws {
        ws.lock()
            .await
            .remove_subscriber_session(&peer.device_public_key, session.outbound);
    }
    subscribed.retain(|id| id != &workspace_id);
    Ok(None)
}
