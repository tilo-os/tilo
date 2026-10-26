//! `Subscribe`, `SubscribeFrom` and `Unsubscribe`: admission, resume logs,
//! replay capture and live-subscriber registration.

use super::{
    Arc, AuthenticatedPeer, Decision, Envelope, Lag, MAX_AUTOBOUND_WORKSPACES, PeerError,
    ReplayCatchup, ReplayHandle, ReplayTicket, ResumeLog, RoomOperation, SessionCtx,
    SharedWorkspace, StateDelivery, StateSession, StateSource, Step, StoreError, Subscriber,
    WorkspaceAutobind, WorkspaceRegistry, current_session, lost_session, mpsc, room_access,
    session_peer_authorized, unix_now_ns,
};
use tilo_lock::MutexRecover as _;

/// How a subscriber's replay capture finished.
enum SubscribeReplayCompletion {
    Ready(ReplayHandle),
    Superseded,
}

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
    workspace_id: u64,
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

/// A room that admitted a `Subscribe`, with what its admitting membership
/// read showed.
struct AdmittedSubscription {
    workspace: SharedWorkspace,
    /// The subscriber channel the room held for this device when it admitted
    /// the subscription; the replay completion checks it is still the same.
    expected: Option<mpsc::Sender<Envelope>>,
    /// The device's earlier connections still subscribing when this one was
    /// let in: whichever of them registers first, this one replaces.
    earlier: Vec<mpsc::WeakSender<Envelope>>,
    store: crate::store::WorkspaceStore,
}

/// How [`admit_subscription`] answered a `Subscribe`.
enum SubscribeAdmission {
    Admitted(AdmittedSubscription),
    /// Refused without ending the session; the reply to send.
    Refused(Envelope),
}

/// Handle `Subscribe`: gate on membership (or admit, for a
/// same-owner autobind), replay retained baselines + this device's
/// pending queue, then register the live subscriber.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the ticket and replay handle move into the connection's replay slot; the lint does not see the move"
)]
pub(super) async fn handle_subscribe(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<u64>,
    replay: &mut Option<(u64, ReplayTicket, ReplayHandle, Arc<ReplayCatchup>)>,
    workspace_id: u64,
    resume: Subscription,
) -> Result<Option<Envelope>, PeerError> {
    let AdmittedSubscription {
        workspace: ws,
        expected,
        earlier,
        store,
    } = {
        let admission = admit_subscription(peer, session, subscribed, workspace_id).await?;
        match admission {
            SubscribeAdmission::Admitted(admitted) => admitted,
            SubscribeAdmission::Refused(refusal) => return Ok(Some(refusal)),
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
    // A peer relay keeps all of the room; a device opens on-demand records.
    if peer.role == crate::auth::PeerRole::PeerRelay {
        ticket = ticket.with_every_record();
    }
    crate::timing::event("join.ticket_wait", waited, 0);
    let capturing = crate::timing::start();
    let capture = ticket.authority.clone();
    let bind = ticket.authority.clone();
    let catchup = Arc::new(ReplayCatchup::new(&work));
    let registration = SubscriberRegistration {
        workspace: completion_workspace,
        source,
        expected,
        earlier,
        bind,
        presence: session.presence.cloned(),
        state_tx,
        catchup: catchup.clone(),
        lag: session.lag.clone(),
    };

    let rx = owner
        .submit_replay_resumable_wait(
            true,
            1,
            async { Ok(()) },
            move |state, _| {
                let step = crate::timing::windowed("owner.replay.capture_step", || {
                    state.capture_resumed_replay(&capture, skip.clone())
                })?;
                match step {
                    Step::More => Ok(Step::More),
                    Step::Done(handle) => Ok(Step::Done(SubscribeReplayCompletion::Ready(handle))),
                }
            },
            move |result| async move {
                let SubscribeReplayCompletion::Ready(handle) = result? else {
                    return Err(crate::persistence::error(
                        "only replay completion can classify a subscriber as superseded",
                    ));
                };
                register_subscriber(registration, handle).await
            },
        )
        .await?;
    let completion = crate::persistence::receipt(rx).await?;
    crate::timing::event("join.capture", capturing, 0);
    let handle = match completion {
        SubscribeReplayCompletion::Ready(handle) => handle,
        SubscribeReplayCompletion::Superseded => return Err(PeerError::Superseded),
    };
    // The device governing a room served here is back: this host's own
    // session to it is dialled now, not once its dial loop's backoff ends.
    if let Some((governor, wake)) = &ws.lock().await.governor_dial
        && *governor == peer.device_public_key
    {
        tracing::info!(
            workspace_id,
            "the governor of a room served here is back; dialling it now"
        );
        wake.notify_one();
    }
    *replay = Some((workspace_id, ticket, handle, catchup));
    session.metrics.inc_subscribes();
    if !subscribed.contains(&workspace_id) {
        subscribed.push(workspace_id);
    }
    Ok(None)
}

/// The admission half of [`handle_subscribe`]: the agent's delegation, the
/// room's binding (autobinding it for the relay owner's own devices), and
/// the room's decision under a fresh membership read, enrolling the device
/// when the room says so. A connection whose own authorization changed
/// meanwhile ends.
async fn admit_subscription(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    workspace_id: u64,
) -> Result<SubscribeAdmission, PeerError> {
    // An agent outside its delegation never gets a room bound for it.
    if let Err(reason) = session
        .trust()
        .agent(&peer.device_public_key, workspace_id, unix_now_ns())
    {
        return Ok(SubscribeAdmission::Refused(Envelope::NotAuthorised {
            workspace_id,
            reason: reason.reason().into(),
        }));
    }
    // Only the relay owner's own devices can be enrolled into an autobound
    // room, so only they may cause one to be bound.
    let autobind = session
        .autobind
        .filter(|_| peer.user_public_key == *session.own_user_public_key);
    let ws = get_or_autobind_workspace(session.workspaces, autobind, workspace_id).await?;
    let Some(ws) = ws else {
        return Ok(SubscribeAdmission::Refused(Envelope::NotAuthorised {
            workspace_id,
            reason: "workspace not bound on this relay".into(),
        }));
    };
    let mut wr = crate::workspace_io::fresh_membership(&ws).await?;
    if !session_peer_authorized(peer, session) {
        return Err(PeerError::Protocol(
            "subscriber authorization changed".into(),
        ));
    }
    if subscribed.contains(&workspace_id) && !current_session(&wr, peer, session.outbound) {
        return Err(lost_session(&wr, peer, session, RoomOperation::Subscribe));
    }
    // Same-owner peer relays are gated by their active federation lease, so
    // they don't need to appear in the workspace membership list.
    match session.decide(&wr, peer, RoomOperation::Subscribe) {
        Decision::Admit => {}
        Decision::AdmitAndEnrol => {
            let revision = wr.membership_revision;
            drop(wr);
            crate::workspace_io::admit_member(&ws, peer.device_public_key, revision).await?;
            wr = crate::workspace_io::fresh_membership(&ws).await?;
            if !session.admits(&wr, peer, RoomOperation::Subscribe) {
                return Err(PeerError::Protocol(
                    "subscriber authorization changed".into(),
                ));
            }
            if subscribed.contains(&workspace_id) && !current_session(&wr, peer, session.outbound) {
                return Err(lost_session(&wr, peer, session, RoomOperation::Subscribe));
            }
        }
        Decision::Deny(reason) => {
            return Ok(SubscribeAdmission::Refused(Envelope::NotAuthorised {
                workspace_id,
                reason: reason.reason().into(),
            }));
        }
    }
    let expected = wr
        .subscribers
        .get(&peer.device_public_key)
        .map(|sub| sub.tx.clone());
    let earlier = wr.subscription_admitted(peer.device_public_key, session.outbound);
    let store = wr.store.clone();
    drop(wr);
    Ok(SubscribeAdmission::Admitted(AdmittedSubscription {
        workspace: ws,
        expected,
        earlier,
        store,
    }))
}

/// What a subscriber's replay completion registers in the room once its
/// capture is ready.
struct SubscriberRegistration {
    workspace: SharedWorkspace,
    source: StateSource,
    /// The subscriber channel the room held at admission.
    expected: Option<mpsc::Sender<Envelope>>,
    /// The device's connections that were still subscribing at admission.
    earlier: Vec<mpsc::WeakSender<Envelope>>,
    /// The replay ticket's authority, bound to the session's liveness.
    bind: Arc<crate::persistence::ReplayAuthority>,
    presence: Option<Arc<crate::presence_mailbox::PresenceMailbox>>,
    state_tx: mpsc::Sender<StateDelivery>,
    catchup: Arc<ReplayCatchup>,
    lag: Arc<Lag>,
}

/// The completion half of [`handle_subscribe`], run under the room's lock
/// once the replay capture is ready: re-check the subscriber against the
/// room, then bind the ticket and register the live subscriber, its
/// presence route and its state session.
async fn register_subscriber(
    registration: SubscriberRegistration,
    handle: ReplayHandle,
) -> Result<SubscribeReplayCompletion, StoreError> {
    let SubscriberRegistration {
        workspace: completion_workspace,
        source,
        expected,
        earlier,
        bind,
        presence,
        state_tx,
        catchup: session_catchup,
        lag: session_lag,
    } = registration;
    let mut wr = crate::workspace_io::fresh_membership(&completion_workspace).await?;
    wr.subscription_completed(&source.peer.device_public_key, &source.tx);
    let current = wr.subscribers.get(&source.peer.device_public_key);
    // An earlier connection of the device, still subscribing when this one
    // was let in, registered first: the stale session of a device that
    // reconnected. This newer one replaces it.
    let replaces_earlier = current.is_some_and(|current| {
        earlier.iter().any(|earlier| {
            earlier
                .upgrade()
                .is_some_and(|earlier| earlier.same_channel(&current.tx))
        })
    });
    let unchanged = replaces_earlier
        || match (expected.as_ref(), current) {
            (Some(expected), Some(current)) => expected.same_channel(&current.tx),
            (None, None) => true,
            // The device's earlier connection held the slot when this
            // one subscribed, and its teardown has since released it.
            // This subscription would have displaced it anyway, so it
            // takes the slot. Only this connection's own slot
            // vanishing is a loss of authorization.
            (Some(expected), None) => !expected.same_channel(&source.tx),
            (None, Some(_)) => false,
        };
    // The spectator seat is taken here, under the room's lock:
    // of two spectators let in for the last seat, one ends here.
    if !source.alive()
        || !source.admits(&wr, &source.peer)
        || !room_access::seated(&wr, &source.peer)
    {
        return Err(crate::persistence::error(
            "replay subscriber authorization changed",
        ));
    }
    let replaced =
        current.is_some_and(|current| !current.tx.same_channel(&source.tx)) && !unchanged;
    if replaced {
        return Ok(SubscribeReplayCompletion::Superseded);
    }
    if !unchanged {
        return Err(crate::persistence::error(
            "replay subscriber authorization changed",
        ));
    }
    let live = wr
        .subscribers
        .get(&source.peer.device_public_key)
        .filter(|current| current.tx.same_channel(&source.tx))
        .and_then(|_| wr.state_sessions.get(&source.peer.device_public_key))
        .map_or_else(
            || Arc::new(std::sync::atomic::AtomicBool::new(true)),
            |session| session.live.clone(),
        );
    // Binding may fail when the waiting connection/receipt was dropped.
    // It must precede every registration side effect.
    bind.bind(live.clone())?;
    wr.add_subscriber(Subscriber {
        device_public_key: source.peer.device_public_key,
        role: source.peer.role,
        tx: source.tx,
        disconnect: source.disconnect,
    });
    if let Some(mailbox) = presence {
        wr.route_presence(source.peer.device_public_key, mailbox);
    }
    wr.state_sessions.insert(
        source.peer.device_public_key,
        StateSession {
            live,
            peer: source.peer,
            tx: state_tx,
            replay: Some(session_catchup),
            lag: Some(session_lag),
        },
    );
    drop(wr);
    Ok(SubscribeReplayCompletion::Ready(handle))
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
    subscribed: &mut Vec<u64>,
    workspace_id: u64,
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

pub async fn get_or_autobind_workspace(
    workspaces: &WorkspaceRegistry,
    autobind: Option<&WorkspaceAutobind>,
    workspace_id: u64,
) -> Result<Option<SharedWorkspace>, StoreError> {
    let existing = workspaces.lock().await.get(&workspace_id).cloned();
    if let Some(workspace) = existing {
        return Ok(Some(workspace));
    }
    let Some(autobind) = autobind else {
        return Ok(None);
    };
    crate::workspace_io::get_or_initialize(
        workspaces,
        autobind.state_root.clone(),
        &autobind.shell_payloads,
        workspace_id,
        true,
        crate::room_access::AdmissionMode::SameOwner,
        MAX_AUTOBOUND_WORKSPACES,
    )
    .await
    .map(Some)
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
mod autobind_tests {
    use super::super::{AsyncMutex, HashMap, autobind_has_capacity};
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn autobind_open_releases_registry_lock_and_shares_one_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Arc::new(AsyncMutex::new(HashMap::new()));
        let autobind = WorkspaceAutobind {
            state_root: tmp.path().to_path_buf(),
            shell_payloads: crate::payloads::neighbour(),
        };
        let (release, reached) =
            crate::store::test_barrier::install(&tmp.path().join("workspaces/7"), "open");
        let observer_registry = registry.clone();
        let handle = tokio::runtime::Handle::current();
        let observer = std::thread::spawn(move || {
            handle.block_on(async move {
                reached.await.unwrap();
                let unlocked =
                    tokio::time::timeout(tilo_test_env::hang_bound(), observer_registry.lock())
                        .await
                        .is_ok();
                drop(release);
                unlocked
            })
        });
        let (first, second) = tokio::join!(
            get_or_autobind_workspace(&registry, Some(&autobind), 7),
            get_or_autobind_workspace(&registry, Some(&autobind), 7)
        );
        assert!(
            observer.join().unwrap(),
            "workspace open held registry lock"
        );
        assert!(Arc::ptr_eq(
            &first.unwrap().unwrap(),
            &second.unwrap().unwrap()
        ));
        assert_eq!(registry.lock().await.len(), 1);
    }

    #[test]
    fn automatic_workspace_creation_has_a_hard_capacity_bound() {
        assert!(autobind_has_capacity(MAX_AUTOBOUND_WORKSPACES - 1));
        assert!(!autobind_has_capacity(MAX_AUTOBOUND_WORKSPACES));
        assert!(!autobind_has_capacity(usize::MAX));
    }
}
