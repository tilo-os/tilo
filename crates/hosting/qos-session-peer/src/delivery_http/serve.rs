//! Installed native data routes, dispatched only by the serialized host owner.
//! Every request is admitted with its original proof and a turn reserved on the
//! ingress pool; held polls retain their original charged request.
use super::data::owner as cores;
use super::*;
use crate::{
    host_bytes::HostBytes,
    https::{BufferedRequest, Route},
    owner::resources::OwnerDataOperation,
};
use hyper::StatusCode;

/// Whether a request was answered now or retained as a held poll.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Dispatched {
    Responded,
    Held,
}

/// Data capabilities the installed native host advertises and serves.
pub(crate) const INSTALLED_DATA_CAPABILITIES: u64 =
    tilo_relay::delivery_v3::capabilities::PRODUCT_DATA_V1;

pub(crate) struct DataRoutes {
    polls: poll::Polls,
    /// PIPELINED_POLL_V1 chain position per authenticated session.
    chains: std::collections::BTreeMap<[u8; 16], ChainState>,
    uploads: staged::Uploads,
    snapshots: snapshots::Snapshots,
    private: private::Private,
    admitted: Option<AdmittedSink>,
    idle: idle_margin::IdleMargin,
}

/// LIVE_V1 hook: receives `(author device, canonical item)` for each newly
/// accepted state item (inline or staged), after every admission check and
/// immediately before its durable store barrier (forward-before-commit, see
/// `crate::owner::delivery::early_forward`). Must not block: the live
/// forwarder only enqueues and publishes on another task.
pub(crate) type AdmittedSink = crate::owner::delivery::early_forward::EarlySink;

fn refuse(request: BufferedRequest) {
    let _ = request.respond(StatusCode::FORBIDDEN, b"permission-denied".to_vec());
}

/// Byte-pool, disk or native-quota pressure is transient: it is not a
/// statement about the session, so clients must back off and retry it
/// rather than discarding the session and re-enrolling.
pub(crate) fn transient_capacity(error: &anyhow::Error) -> bool {
    use crate::owner::delivery::error::{
        OwnerCapacityKind as Capacity, OwnerDataFailure, OwnerDataFailureKind as Kind,
    };
    error.chain().any(|cause| {
        cause
            .downcast_ref::<OwnerDataFailure>()
            .is_some_and(|failure| {
                matches!(
                    failure.kind(),
                    Kind::Capacity(Capacity::Bytes | Capacity::Disk | Capacity::NativeQuota)
                )
            })
            || matches!(
                cause.downcast_ref::<crate::delivery_http::upload::UploadFailure>(),
                Some(crate::delivery_http::upload::UploadFailure::Capacity)
            )
    })
}

/// Opaque refusal, except transient capacity which answers a retryable 503.
fn refuse_error(request: BufferedRequest, error: &anyhow::Error) {
    if transient_capacity(error) {
        let _ = request.respond(
            StatusCode::SERVICE_UNAVAILABLE,
            b"temporarily-unavailable".to_vec(),
        );
    } else {
        refuse(request)
    }
}

/// One SnapshotRequired frame when replay failed only because the consumer's
/// cursor must restart (for example after an authorization-view change).
fn reset_frame(owner: &SessionOwner, error: &anyhow::Error) -> Option<Vec<u8>> {
    let reason = snapshots::reset_reason(error)?;
    snapshots::Snapshots::required(owner, reason).ok()
}

/// Chain states kept at once (live sessions are bounded far lower).
const CHAIN_STATES: usize = 64;

/// PIPELINED_POLL_V1 position of one session's current chain.
struct ChainState {
    id: u64,
    /// The index answered next; later indexes wait.
    next: u32,
    /// Signed cursor ending the chain's last served page.
    floor: Option<tilo_relay::delivery_v3::cursor::CursorToken>,
}

enum ChainGate {
    Ready(Option<tilo_relay::delivery_v3::cursor::CursorToken>),
    Wait,
    Stale,
}

impl DataRoutes {
    pub(crate) fn new(work: crate::store::HostWorkInventory) -> Self {
        Self {
            polls: poll::Polls::new(),
            chains: std::collections::BTreeMap::new(),
            uploads: staged::Uploads::new(work.clone()),
            snapshots: snapshots::Snapshots::new(work),
            private: private::Private::new(),
            admitted: None,
            idle: idle_margin::IdleMargin::new(),
        }
    }

    /// Install the LIVE_V1 admitted-change sink (inline and staged items).
    pub(crate) fn install_admitted_sink(&mut self, sink: AdmittedSink) {
        self.admitted = Some(sink);
    }

    /// Routes owned by the data carrier. Media and bootstrap routes stay with
    /// the media ingress dispatcher.
    pub(crate) fn owns(route: Route) -> bool {
        matches!(
            route,
            Route::State
                | Route::Acks
                | Route::EventsPoll
                | Route::Detach
                | Route::Snapshot
                | Route::SnapshotChunk
                | Route::Private(_)
                | Route::PrivatePoll
        )
    }

    /// Finish a data session: register its durable consumer before Ready.
    /// Media-only sessions keep the capability-zero finish path.
    pub(crate) fn finish_session(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &mut SessionOwner,
        budget: &HostBytes,
        body: &[u8],
    ) -> Result<Vec<u8>> {
        let intent = sessions.finish_intent(body)?;
        if intent.requested() & sessions.capabilities == 0 {
            return sessions.finish_current(owner, current, body);
        }
        let ready = sessions.finish_data(owner, current, budget, body)?;
        for retired in &ready.retired {
            self.polls.fence(*retired);
            self.chains.remove(retired);
        }
        Ok(ready.bytes)
    }

    pub(crate) fn handle(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &mut SessionOwner,
        budget: &HostBytes,
        request: BufferedRequest,
    ) -> Dispatched {
        if request.is_cancelled() {
            return Dispatched::Responded;
        }
        match request.route() {
            Route::Private(_) => {
                if let Err(request) = self
                    .private
                    .command(sessions, current, owner, budget, request)
                {
                    refuse(request);
                }
                return Dispatched::Responded;
            }
            Route::PrivatePoll => {
                return match self.private.poll(sessions, current, owner, budget, request) {
                    Ok(()) => Dispatched::Held,
                    Err(request) => {
                        refuse(request);
                        Dispatched::Responded
                    }
                };
            }
            _ => {}
        }
        // Snapshot manifests own their request: the snapshot module may answer
        // now or hand hashing/staging off the owner loop and answer from pump().
        if request.route() == Route::Snapshot {
            if let Err(request) = self
                .snapshots
                .start(sessions, current, owner, budget, request)
            {
                refuse(request);
            }
            return Dispatched::Responded;
        }
        if request.route() == Route::State && staged::partial_chunk(request.body()).is_some() {
            if let Err(request) = self
                .uploads
                .start(sessions, current, owner, budget, request)
            {
                refuse(request);
            }
            return Dispatched::Responded;
        }
        let result = match request.route() {
            Route::State => self.state(sessions, current, owner, budget, &request),
            Route::Acks => self
                .ack(sessions, current, owner, budget, &request)
                .map(|()| Some(Vec::new())),
            Route::Detach => self.detach(sessions, current, owner, &request).map(Some),
            Route::SnapshotChunk => self
                .snapshots
                .chunk(sessions, current, owner, budget, &request)
                .map(Some),
            Route::EventsPoll => {
                return match self.poll(sessions, current, owner, budget, request) {
                    Ok(dispatched) => dispatched,
                    Err(request) => {
                        refuse(request);
                        Dispatched::Responded
                    }
                };
            }
            _ => Err(anyhow::anyhow!("not a data route")),
        };
        match result {
            Ok(Some(body)) => {
                let _ = request.respond(StatusCode::OK, body);
            }
            Ok(None) => refuse(request),
            Err(error) => {
                // Clients see only the opaque denial; operators see the cause.
                tracing::debug!(route = ?request.route(), %error, "data request refused");
                refuse_error(request, &error)
            }
        }
        Dispatched::Responded
    }

    fn state(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &mut SessionOwner,
        budget: &HostBytes,
        request: &BufferedRequest,
    ) -> Result<Option<Vec<u8>>> {
        let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
        let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
        let items = super::data::prepare_state(&exact)
            .map_err(|error| anyhow::anyhow!("invalid bounded state request: {error:?}"))?;
        ensure!(!items.is_empty(), "empty state request");
        // One reservation covers the whole batch: consecutive new document
        // items commit as one replay group under this first turn.
        let total = items
            .iter()
            .try_fold(0usize, |sum, item| sum.checked_add(item.canonical.len()))
            .context("state batch length overflow")?;
        let (admitted, mut checker, turn) = sessions.admit_revalidating(
            owner,
            current,
            budget,
            OwnerDataOperation::State { input_len: total },
            &proof,
            &exact,
            request,
        )?;
        drop(items);
        let response =
            crate::owner::delivery::early_forward::scoped(self.admitted.as_ref(), || {
                cores::admit_state_core(
                    owner,
                    current,
                    &admitted,
                    |owner, view, session| checker.check(owner, view, session),
                    budget,
                    turn,
                )
            })?;
        Ok(Some(response))
    }

    fn ack(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &mut SessionOwner,
        budget: &HostBytes,
        request: &BufferedRequest,
    ) -> Result<()> {
        let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
        let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
        let (admitted, mut checker, turn) = sessions.admit_revalidating(
            owner,
            current,
            budget,
            OwnerDataOperation::Ack,
            &proof,
            &exact,
            request,
        )?;
        cores::ack_core(
            owner,
            current,
            &admitted,
            |owner, view, session| checker.check(owner, view, session),
            turn,
        )
    }

    fn detach(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &SessionOwner,
        request: &BufferedRequest,
    ) -> Result<Vec<u8>> {
        let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
        let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
        let id = {
            let admitted = sessions.admit_current(owner, current, &proof, &exact)?;
            admitted.session().session_id()
        };
        let named = request
            .target()
            .strip_prefix("/peer/v3/sessions/")
            .and_then(|hex| hex::decode(hex).ok())
            .context("invalid detach target")?;
        ensure!(named == id, "detach names another session");
        sessions.retire_session(id);
        self.polls.fence(id);
        self.chains.remove(&id);
        self.private.retire_session(id);
        Ok(Vec::new())
    }

    /// Answer a poll now when a complete group exists; otherwise hold the
    /// original request until a commit, cancellation or its deadline.
    /// A PIPELINED_POLL_V1 chain poll is answered only in chain order.
    fn poll(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &SessionOwner,
        budget: &HostBytes,
        request: BufferedRequest,
    ) -> std::result::Result<Dispatched, BufferedRequest> {
        enum Arrival {
            Page(cores::ServedPage),
            Hold,
            Stale,
        }
        let mut position = None;
        let outcome = (|| -> Result<(Arrival, Option<poll::AdmissionTicket>)> {
            let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
            let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
            let (admitted, mut checker, turn) = sessions.admit_revalidating(
                owner,
                current,
                budget,
                OwnerDataOperation::Replay,
                &proof,
                &exact,
                &request,
            )?;
            let entry = checker.entry_serial();
            let ticket = self
                .polls
                .capture(&admitted, &request, entry)
                .map_err(|error| anyhow::anyhow!("poll capture refused: {error:?}"))?;
            let session = admitted.session().session_id();
            position = ticket.chain().map(|chain| (session, chain));
            self.polls.supersede(&ticket);
            match ticket.chain() {
                // A plain poll ends any chain of this session.
                None => {
                    self.chains.remove(&session);
                }
                Some(chain) if chain.index == 0 => {
                    self.start_chain(session, chain.chain);
                }
                Some(_) => {}
            }
            let floor = match self.chain_gate(session, ticket.chain()) {
                ChainGate::Ready(floor) => floor,
                ChainGate::Wait => return Ok((Arrival::Hold, Some(ticket))),
                ChainGate::Stale => return Ok((Arrival::Stale, None)),
            };
            let page = cores::replay_core_from(
                owner,
                current,
                &admitted,
                floor.as_ref(),
                |owner, view, session| checker.check(owner, view, session),
                turn,
            )?;
            Ok(match page {
                Some(page) => (Arrival::Page(page), None),
                None => (Arrival::Hold, Some(ticket)),
            })
        })();
        match outcome {
            Ok((Arrival::Page(page), _)) => {
                if let Some((session, chain)) = position {
                    self.chain_answered(session, chain, Some(page.end));
                }
                let _ = request.respond(StatusCode::OK, page.bytes);
                Ok(Dispatched::Responded)
            }
            // Another chain, an index already answered, or a chain this host
            // no longer tracks. Not an empty page (which a live chain would
            // accept and follow): a transient refusal, so the client abandons
            // the chain and starts a new one at its own cursor.
            Ok((Arrival::Stale, _)) => {
                let _ = request.respond(StatusCode::CONFLICT, b"stale-poll-chain".to_vec());
                Ok(Dispatched::Responded)
            }
            Ok((Arrival::Hold, Some(ticket))) => match self.polls.park(ticket, request) {
                Ok(_) => Ok(Dispatched::Held),
                // A full table or chain: the dropped request answers 503 and
                // the client polls again (a chain client starts a new chain).
                Err(_) => Ok(Dispatched::Responded),
            },
            Ok((Arrival::Hold, None)) => Err(request),
            Err(error) => match reset_frame(owner, &error) {
                // An authenticated consumer whose cursor can no longer be
                // served receives the contract's reset, not a denial.
                Some(frame) => {
                    tracing::debug!(route = ?request.route(), %error, "data poll reset");
                    if let Some((session, _)) = position {
                        self.chains.remove(&session);
                    }
                    let _ = request.respond(StatusCode::OK, frame);
                    Ok(Dispatched::Responded)
                }
                None if transient_capacity(&error) => {
                    tracing::debug!(route = ?request.route(), %error, "data poll deferred");
                    refuse_error(request, &error);
                    Ok(Dispatched::Responded)
                }
                None => {
                    tracing::debug!(route = ?request.route(), %error, "data poll refused");
                    Err(request)
                }
            },
        }
    }

    /// Begin chain `id` for `session` (its index 0 arrived).
    fn start_chain(&mut self, session: [u8; 16], id: u64) {
        // Bounded: sessions without held polls lose their chain first (a
        // later index of such a chain is then answered empty and the client
        // starts a new chain).
        if !self.chains.contains_key(&session) && self.chains.len() >= CHAIN_STATES {
            let held = self.polls.sessions();
            self.chains.retain(|key, _| held.contains(key));
        }
        if self.chains.contains_key(&session) || self.chains.len() < CHAIN_STATES {
            self.chains.insert(
                session,
                ChainState {
                    id,
                    next: 0,
                    floor: None,
                },
            );
        }
    }

    /// Whether a (chain) poll may be served now, and after which cursor.
    fn chain_gate(
        &self,
        session: [u8; 16],
        chain: Option<tilo_relay::delivery_v3::poll_chain::ChainPoll>,
    ) -> ChainGate {
        let Some(chain) = chain else {
            return ChainGate::Ready(None);
        };
        match self.chains.get(&session) {
            Some(state) if state.id == chain.chain && chain.index == state.next => {
                ChainGate::Ready(state.floor.clone())
            }
            Some(state) if state.id == chain.chain && chain.index > state.next => ChainGate::Wait,
            _ => ChainGate::Stale,
        }
    }

    /// Record that chain position `chain` was answered (with a page ending at
    /// `end`, or empty).
    fn chain_answered(
        &mut self,
        session: [u8; 16],
        chain: tilo_relay::delivery_v3::poll_chain::ChainPoll,
        end: Option<tilo_relay::delivery_v3::cursor::CursorToken>,
    ) {
        let Some(state) = self.chains.get_mut(&session) else {
            return;
        };
        if state.id != chain.chain || state.next != chain.index {
            return;
        }
        match chain.index.checked_add(1) {
            Some(next) => {
                state.next = next;
                if end.is_some() {
                    state.floor = end;
                }
            }
            None => {
                self.chains.remove(&session);
            }
        }
    }

    /// Resume every held poll against the current committed owner. Called
    /// after any owner commit and on the maintenance tick. Held polls are
    /// visited in arrival order, so a chain's earlier poll is answered (and
    /// moves the chain on) before its later polls are considered.
    pub(crate) fn wake(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &SessionOwner,
        budget: &HostBytes,
    ) {
        self.polls.prune_cancelled();
        let now = tokio::time::Instant::now();
        for id in self.polls.wake_ids() {
            let Some(pending) = self.polls.take(id) else {
                continue;
            };
            let deadline = pending.original().deadline();
            let session = pending.session_id();
            let chain = pending.chain();
            let floor = match self.chain_gate(session, chain) {
                ChainGate::Ready(floor) => floor,
                ChainGate::Wait => {
                    // Its predecessor is still unanswered. An idle answer
                    // here does not move the chain.
                    if !self.idle.may_hold(deadline, now) {
                        let _ = pending.into_request().respond(StatusCode::OK, Vec::new());
                    } else if let Err(error) = self.polls.repark(pending) {
                        tracing::debug!(?error, "held chain poll could not be retained");
                    }
                    continue;
                }
                ChainGate::Stale => {
                    let _ = pending.into_request().respond(StatusCode::OK, Vec::new());
                    continue;
                }
            };
            let result = (|| -> Result<Option<cores::ServedPage>> {
                let (session, mut checker, turn) =
                    sessions.resume_poll(owner, current, budget, &pending)?;
                cores::replay_pending_core_from(
                    owner,
                    current,
                    &pending,
                    &session,
                    floor.as_ref(),
                    |owner, view, session| checker.check(owner, view, session),
                    turn,
                )
            })();
            match result {
                Ok(Some(page)) => {
                    if let Some(chain) = chain {
                        self.chain_answered(session, chain, Some(page.end));
                    }
                    let _ = pending.into_request().respond(StatusCode::OK, page.bytes);
                }
                Ok(None) if self.idle.may_hold(deadline, now) => {
                    if let Err(error) = self.polls.repark(pending) {
                        tracing::debug!(?error, "held poll could not be retained");
                    }
                }
                // Deadline reached with nothing new: an empty page means idle.
                Ok(None) => {
                    if let Some(chain) = chain {
                        self.chain_answered(session, chain, None);
                    }
                    let _ = pending.into_request().respond(StatusCode::OK, Vec::new());
                }
                Err(error) => match reset_frame(owner, &error) {
                    Some(frame) => {
                        tracing::debug!(%error, "held data poll reset");
                        self.chains.remove(&session);
                        let _ = pending.into_request().respond(StatusCode::OK, frame);
                    }
                    None => {
                        tracing::debug!(%error, "held data poll refused");
                        refuse_error(pending.into_request(), &error)
                    }
                },
            }
        }
    }

    pub(crate) fn next_deadline(&self) -> Option<tokio::time::Instant> {
        self.polls.next_deadline(self.idle.margin())
    }

    /// Answer every held poll (private events and data) that would reach its
    /// idle-answer window within `horizon`, before the owner runs a known long
    /// turn (delivery retention rotation can hold the owner for ~1 s, which
    /// otherwise lets the HTTPS deadline answer 503 first).
    pub(crate) fn answer_idle_within(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &SessionOwner,
        budget: &HostBytes,
        horizon: std::time::Duration,
    ) {
        self.idle.set_floor(Some(horizon));
        self.private
            .pump(sessions, current, owner, budget, &self.idle);
        self.wake(sessions, current, owner, budget);
        self.idle.set_floor(None);
    }

    /// Record one owner-loop turn boundary; held polls with nothing ready are
    /// answered idle one observed slow turn ahead of their HTTPS deadline.
    pub(crate) fn observe_turn(&mut self, now: tokio::time::Instant) {
        self.idle.observe(now);
    }

    /// Whether handed-off work (disk staging) awaits completion on the owner.
    pub(crate) fn has_pending_work(&self) -> bool {
        self.uploads.busy() || self.private.busy() || self.snapshots.busy()
    }

    /// Whether a snapshot pin was issued or renewed within `window_ns`, so a
    /// client download may still be in progress. Delivery retention defers
    /// optional baseline replacement and rotation while this holds.
    pub(crate) fn snapshot_recently_pinned(&self, now_ns: u64, window_ns: u64) -> bool {
        self.snapshots.recently_pinned(now_ns, window_ns)
    }

    /// Follow the host's current action runtime for private routes.
    pub(crate) fn sync_private(&mut self, actions: Option<&crate::action_host::ActionHost>) {
        self.private.sync(actions);
    }

    /// Complete handed-off work in the serialized owner. Returns whether an
    /// owner commit may have occurred, so the caller wakes held polls.
    pub(crate) fn pump(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &mut SessionOwner,
        budget: &HostBytes,
    ) -> bool {
        let sink = self.admitted.clone();
        let committed = crate::owner::delivery::early_forward::scoped(sink.as_ref(), || {
            self.uploads.pump(sessions, current, owner, budget)
        });
        self.private
            .pump(sessions, current, owner, budget, &self.idle);
        self.snapshots.pump(sessions, current, owner, budget);
        committed
    }
}

mod idle_margin;
mod private;
mod snapshots;
mod staged;

#[cfg(test)]
mod tests;
