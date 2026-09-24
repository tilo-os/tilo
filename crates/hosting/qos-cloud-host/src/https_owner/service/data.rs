//! State, acknowledgement and held event polls over reserved Cloud Store turns.
//! A held poll retains only its original charged request and continuation;
//! each resume re-derives the session, policy and page from a fresh turn.
use super::*;
use qos_relay::delivery_v3::{
    auth::VerifiedSession,
    commit::{CommitRef, DeliveryId, Digest},
    cursor::CursorToken,
    framing::{self, FrameError, MessageType},
    http_proof::Method,
    state::{
        self, CommitBoundary, CursorAck, EventChunk, EventKind, EventSource, StateItemChunk,
        visible_order_digest,
    },
};
use qos_session_peer::ingress::PendingCarrier;

const PAGE_BYTES: usize = 1024 * 1024;
const PAGE_FRAMES: usize = 64;
/// Leave fixed metadata space below the encoded 256 KiB frame cap.
const EVENT_CHUNK: usize = 240 * 1024;
/// A held poll answers idle this long before its original deadline.
const IDLE_MARGIN: Duration = Duration::from_millis(100);
/// Never more held polls than the registry's live-session bound.
const MAX_POLLS: usize = 16;

pub(super) struct PreparedItem<'a> {
    pub(super) delivery: DeliveryId,
    pub(super) canonical: &'a [u8],
}
fn frame_error(_: FrameError) -> anyhow::Error {
    anyhow!("invalid delivery frame")
}
/// Complete offset-zero items only; partial chunks take the staged path.
pub(super) fn prepare_state<'a>(request: &'a Request<'_>) -> Result<Vec<PreparedItem<'a>>> {
    ensure!(
        request.method() == Method::Post
            && request.target() == "/peer/v3/state"
            && !request.body().is_empty()
            && request.body().len() <= PAGE_BYTES,
        "invalid bounded state request"
    );
    let mut remaining = request.body();
    let mut items = Vec::new();
    while !remaining.is_empty() {
        ensure!(items.len() < 64, "state batch exceeds bound");
        let (kind, payload, rest) = framing::decode_frame_prefix(remaining).map_err(frame_error)?;
        ensure!(
            kind == MessageType::StateItemChunk,
            "state item frame required"
        );
        let item = StateItemChunk::decode(payload)?;
        ensure!(
            item.offset() == 0
                && item.canonical_size() == item.bytes().len() as u64
                && item.input_digest() == *blake3::hash(item.bytes()).as_bytes(),
            "incomplete or mismatched state item"
        );
        items.push(PreparedItem {
            delivery: item.delivery(),
            canonical: item.bytes(),
        });
        remaining = rest;
    }
    Ok(items)
}
fn prepare_ack(request: &Request<'_>) -> Result<CursorAck> {
    ensure!(
        request.method() == Method::Post
            && request.target() == "/peer/v3/acks"
            && request.body().len() <= 8 * 1024,
        "invalid bounded ACK request"
    );
    let (kind, payload, rest) =
        framing::decode_frame_prefix(request.body()).map_err(frame_error)?;
    ensure!(
        kind == MessageType::CursorAck && rest.is_empty(),
        "one ACK frame required"
    );
    Ok(CursorAck::decode(payload)?)
}
pub(super) fn prepare_poll(method: &str, target: &str, body: &[u8]) -> Result<Option<CursorToken>> {
    ensure!(method == "GET" && body.is_empty(), "invalid poll request");
    if target == "/peer/v3/events?mode=poll" {
        return Ok(None);
    }
    let cursor = target
        .strip_prefix("/peer/v3/events?cursor=")
        .and_then(|value| value.strip_suffix("&mode=poll"))
        .context("invalid poll target")?;
    Ok(Some(state::decode_cursor_url(cursor)?))
}
pub(super) fn frame(kind: MessageType, payload: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    framing::append_frame(&mut out, kind, payload, PAGE_BYTES)?;
    Ok(out)
}

impl CloudService {
    /// Items admit in request order in commit units: consecutive newly
    /// accepted document items share one Store commit and replay group, any
    /// other item commits alone. An earlier unit may change authority, so each
    /// later unit re-validates the same session in its own fresh turn.
    pub(super) fn state(&mut self, request: &BufferedRequest) -> Result<Vec<u8>> {
        let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
        let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
        let items = prepare_state(&exact)?;
        ensure!(!items.is_empty(), "empty state request");
        let items = items
            .iter()
            .map(|item| (item.delivery, item.canonical))
            .collect::<Vec<_>>();
        let length = |items: &[(DeliveryId, &[u8])]| {
            items
                .iter()
                .try_fold(0usize, |sum, (_, canonical)| {
                    sum.checked_add(canonical.len())
                })
                .context("state batch length overflow")
        };
        let sink = self.early_sink();
        let pins = self.pins;
        let registry = &mut self.registry;
        let mut response = Vec::new();
        let mut append = |admissions: Vec<state::StateAdmission>| -> Result<()> {
            for admission in admissions {
                framing::append_frame(
                    &mut response,
                    MessageType::StateAdmission,
                    &admission.encode()?,
                    PAGE_BYTES,
                )?;
            }
            Ok(())
        };
        // LIVE_V1 lane C: each newly accepted item is offered from inside its
        // Store commit, after every admission check and before the barrier
        // (`crate::delivery::early_forward`); the offer only enqueues.
        let (session, admissions) =
            with_state_turn(&self.source, &self.pool, length(&items)?, |turn| {
                let intent = registry.session_intent(proof.session_id())?;
                let context = turn.carrier_context(
                    &pins,
                    intent.device(),
                    intent.requested(),
                    intent.mandatory(),
                )?;
                let admitted = registry.admit_in(&proof, &exact, &context.verification())?;
                drop(context);
                let admissions = crate::delivery::early_forward::scoped(sink.as_ref(), || {
                    turn.admit_state_group(admitted.session(), &items)
                })
                .map_err(delivery)?;
                Ok((admitted.session().session_id(), admissions))
            })?;
        let mut next = admissions.len();
        ensure!(
            next > 0 && next <= items.len(),
            "state commit unit admitted no item"
        );
        append(admissions)?;
        while next < items.len() {
            let rest = &items[next..];
            let admissions = with_state_turn(&self.source, &self.pool, length(rest)?, |turn| {
                let intent = registry.session_intent(session)?;
                let context = turn.carrier_context(
                    &pins,
                    intent.device(),
                    intent.requested(),
                    intent.mandatory(),
                )?;
                let current = registry.revalidate_in(session, &context.verification())?;
                drop(context);
                crate::delivery::early_forward::scoped(sink.as_ref(), || {
                    turn.admit_state_group(current, rest)
                })
                .map_err(delivery)
            })?;
            ensure!(
                !admissions.is_empty() && admissions.len() <= rest.len(),
                "state commit unit admitted no item"
            );
            next += admissions.len();
            append(admissions)?;
        }
        Ok(response)
    }

    pub(super) fn ack(&mut self, request: &BufferedRequest) -> Result<()> {
        let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
        let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
        let ack = prepare_ack(&exact)?;
        let pins = self.pins;
        let registry = &mut self.registry;
        with_turn(
            &self.source,
            &self.pool,
            DeliveryOperation::Acknowledge,
            |turn| {
                let intent = registry.session_intent(proof.session_id())?;
                let context = turn.carrier_context(
                    &pins,
                    intent.device(),
                    intent.requested(),
                    intent.mandatory(),
                )?;
                let admitted = registry.admit_in(&proof, &exact, &context.verification())?;
                drop(context);
                turn.acknowledge_delivery_cursor(admitted.session(), ack.cursor())
                    .map_err(delivery)
            },
        )
    }

    /// Answer now when a complete group exists; otherwise hold the original
    /// request until a commit, cancellation, session fence or its deadline.
    pub(super) fn poll(&mut self, request: BufferedRequest) {
        let outcome = (|| -> Result<(Option<Vec<u8>>, Option<Held>)> {
            let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
            let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
            let cursor = prepare_poll(request.method().as_str(), request.target(), request.body())?;
            let pins = self.pins;
            let registry = &mut self.registry;
            with_turn(
                &self.source,
                &self.pool,
                DeliveryOperation::Replay,
                |turn| {
                    let intent = registry.session_intent(proof.session_id())?;
                    let context = turn.carrier_context(
                        &pins,
                        intent.device(),
                        intent.requested(),
                        intent.mandatory(),
                    )?;
                    let (admitted, ticket) = registry.admit_buffered_in(
                        &proof,
                        &exact,
                        &request,
                        &context.verification(),
                    )?;
                    let session = admitted.session();
                    Ok(match replay_page(turn, session, cursor.as_ref())? {
                        Some(page) => (Some(page), None),
                        None => (
                            None,
                            Some(Held {
                                device: session.device_certificate().device_public_key,
                                session: session.session_id(),
                                cursor,
                                ticket,
                            }),
                        ),
                    })
                },
            )
        })();
        match outcome {
            Ok((Some(page), _)) => {
                let _ = request.respond(StatusCode::OK, page);
            }
            Ok((None, Some(held))) => {
                if let Err(request) = self.polls.park(held, request) {
                    // A second held poll for one consumer, or a full table:
                    // answer idle so the client simply polls again.
                    let _ = request.respond(StatusCode::OK, Vec::new());
                }
            }
            Ok((None, None)) => refuse(request),
            Err(error) => {
                tracing::debug!(%error, "Cloud event poll refused");
                refuse(request)
            }
        }
    }
}

/// Build a page of complete signed replay groups strictly after the verified
/// cursor: exactly one group, or with MULTI_GROUP_REPLAY_V1 several in commit
/// order within the page bounds. An oversized first group returns no prefix
/// and never advances its cursor.
pub(super) fn replay_page(
    turn: &CloudStoreTurn,
    session: &VerifiedSession,
    cursor: Option<&CursorToken>,
) -> Result<Option<Vec<u8>>> {
    let selection = match turn.next_delivery_replay(session, cursor) {
        Ok(Some(selection)) => selection,
        Ok(None) => return Ok(None),
        // A cursor the current history cannot continue (store rotation,
        // authorization change, released history) is answered with one
        // correlated SnapshotRequired frame, as on the native host; it never
        // advances the cursor and the client restarts from a snapshot.
        Err(CloudDeliveryError::Reset(reason)) => {
            return Ok(Some(frame(
                MessageType::SnapshotRequired,
                &state::SnapshotRequired::new(reason, turn.delivery_ledger().store_generation)
                    .encode()?,
            )?));
        }
        Err(error) => return Err(delivery(error)),
    };
    let (max_groups, max_frames) =
        qos_relay::delivery_v3::capabilities::page_bounds(session.capabilities());
    let mut page = Vec::new();
    let (mut groups, mut frames) = (0usize, 0usize);
    let mut next = Some(selection);
    while let Some(selection) = next.take() {
        let committed = selection.committed().clone();
        let group = match replay_group(turn, session, selection) {
            Ok(group) => group,
            Err(error) if groups == 0 => return Err(error),
            // A later group simply starts the next page.
            Err(_) => break,
        };
        if page.len().saturating_add(group.0.len()) > PAGE_BYTES
            || frames.saturating_add(group.1) > max_frames
        {
            break;
        }
        page.try_reserve(group.0.len())?;
        page.extend_from_slice(&group.0);
        frames += group.1;
        groups += 1;
        if groups < max_groups {
            next = turn
                .next_delivery_replay_after(session, &committed)
                .ok()
                .flatten();
        }
    }
    Ok((groups > 0).then_some(page))
}
/// One complete signed group (`EventChunk* CommitBoundary`) and its frames.
fn replay_group(
    turn: &CloudStoreTurn,
    session: &VerifiedSession,
    selection: qos_session_peer::ingress::delivery_planning::ReplaySelection<'_>,
) -> Result<(Vec<u8>, usize)> {
    let committed = selection.committed().clone();
    ensure!(
        selection.entry().publications.len() < PAGE_FRAMES,
        "complete replay group exceeds page frame budget"
    );
    let pool = turn.publication_pool();
    let mut events = Vec::new();
    let mut raw = 0usize;
    for (ordinal, publication) in selection.publications(pool).enumerate() {
        let publication = publication?;
        raw = raw
            .checked_add(publication.bytes.len())
            .context("replay size overflow")?;
        ensure!(
            raw <= PAGE_BYTES,
            "complete replay group exceeds page byte budget"
        );
        let kind = match postcard::from_bytes::<qos_peer_state::NeighbourMsg>(publication.bytes)? {
            qos_peer_state::NeighbourMsg::SyncResp { .. } => EventKind::Governance,
            qos_peer_state::NeighbourMsg::AppDocument { .. } => EventKind::State,
            _ => bail!("unsupported public replay family"),
        };
        events.push(Event {
            ordinal: ordinal.try_into()?,
            digest: *blake3::hash(publication.bytes).as_bytes(),
            kind,
            canonical: publication.bytes,
        });
    }
    let order = events
        .iter()
        .map(|event| (event.ordinal, event.digest))
        .collect::<Vec<_>>();
    let token = turn
        .issue_delivery_cursor(session, committed.clone())
        .map_err(delivery)?;
    let ledger = turn.delivery_ledger();
    let consumer = ledger
        .consumers
        .get(&session.device_certificate().device_public_key)
        .context("consumer not registered")?;
    let boundary = CommitBoundary::new(
        consumer.id,
        consumer.authorization_view,
        ledger.store_generation,
        committed.clone(),
        events.len().try_into()?,
        visible_order_digest(&order)?,
        token,
    )?
    .encode()?;
    let mut page_len = framing::HEADER_BYTES
        .checked_add(boundary.len())
        .context("replay size overflow")?;
    let mut frames = 1usize;
    for event in &events {
        event.visit_chunks(&committed, |payload| {
            page_len = page_len
                .checked_add(framing::HEADER_BYTES + payload.len())
                .context("replay size overflow")?;
            frames += 1;
            ensure!(
                page_len <= PAGE_BYTES && frames <= PAGE_FRAMES,
                "complete replay group exceeds page budget"
            );
            Ok(())
        })?;
    }
    let mut page = Vec::new();
    page.try_reserve_exact(page_len)?;
    for event in &events {
        event.visit_chunks(&committed, |payload| {
            framing::append_frame(&mut page, MessageType::EventChunk, payload, page_len)?;
            Ok(())
        })?;
    }
    framing::append_frame(&mut page, MessageType::CommitBoundary, &boundary, page_len)?;
    ensure!(
        page.len() == page_len,
        "replay preflight differs from encoded group"
    );
    Ok((page, frames))
}
struct Event<'a> {
    ordinal: u32,
    digest: Digest,
    kind: EventKind,
    canonical: &'a [u8],
}
impl Event<'_> {
    fn visit_chunks(
        &self,
        committed: &CommitRef,
        mut visit: impl FnMut(&[u8]) -> Result<()>,
    ) -> Result<()> {
        ensure!(!self.canonical.is_empty(), "empty replay publication");
        for (index, bytes) in self.canonical.chunks(EVENT_CHUNK).enumerate() {
            let chunk = EventChunk::new(
                committed.clone(),
                self.ordinal,
                self.digest,
                EventSource::Native,
                self.kind,
                self.canonical.len() as u64,
                (index * EVENT_CHUNK) as u64,
                bytes.to_vec(),
            )?;
            visit(&chunk.encode()?)?;
        }
        Ok(())
    }
}

/// Captured only from a real buffered admission, before the request parks.
pub(super) struct Held {
    device: [u8; 32],
    session: [u8; 16],
    cursor: Option<CursorToken>,
    ticket: qos_session_peer::ingress::CarrierTicket,
}
struct Parked {
    device: [u8; 32],
    session: [u8; 16],
    cursor: Option<CursorToken>,
    pending: PendingCarrier,
}
pub(super) struct Polls {
    parked: Vec<Parked>,
}
impl Polls {
    pub(super) fn new() -> Self {
        Self { parked: Vec::new() }
    }
    fn park(
        &mut self,
        held: Held,
        request: BufferedRequest,
    ) -> std::result::Result<(), BufferedRequest> {
        self.parked
            .retain(|parked| !parked.pending.original().is_cancelled());
        if self.parked.len() >= MAX_POLLS
            || self
                .parked
                .iter()
                .any(|parked| parked.device == held.device)
        {
            return Err(request);
        }
        // Park re-checks the exact original bytes, proof, deadline and channel;
        // a replaced original is rejected by dropping it.
        let Ok(pending) = held.ticket.park(request) else {
            return Ok(());
        };
        self.parked.push(Parked {
            device: held.device,
            session: held.session,
            cursor: held.cursor,
            pending,
        });
        Ok(())
    }
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.parked
            .iter()
            .map(|parked| {
                let deadline = parked.pending.original().deadline();
                deadline.checked_sub(IDLE_MARGIN).unwrap_or(deadline)
            })
            .min()
    }
    pub(super) fn fence(&mut self, session: [u8; 16]) {
        for parked in self.take(|parked| parked.session == session) {
            refuse(parked.pending.into_request());
        }
    }
    pub(super) fn refuse_all(&mut self) {
        for parked in self.take(|_| true) {
            refuse(parked.pending.into_request());
        }
    }
    pub(super) fn idle_all(&mut self) {
        for parked in self.take(|_| true) {
            let _ = parked
                .pending
                .into_request()
                .respond(StatusCode::OK, Vec::new());
        }
    }
    fn take(&mut self, select: impl Fn(&Parked) -> bool) -> Vec<Parked> {
        let (taken, kept) = std::mem::take(&mut self.parked)
            .into_iter()
            .partition(|parked| select(parked));
        self.parked = kept;
        taken
    }
    /// Resume every held poll against a fresh reserved turn.
    pub(super) fn wake(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        registry: &mut Registries,
        pins: &Pins,
    ) {
        let now = Instant::now();
        for parked in std::mem::take(&mut self.parked) {
            if parked.pending.original().is_cancelled() {
                continue;
            }
            let deadline = parked.pending.original().deadline();
            let result = with_turn(source, pool, DeliveryOperation::Replay, |turn| {
                let intent = registry.session_intent(parked.pending.session_id())?;
                let context = turn.carrier_context(
                    pins,
                    intent.device(),
                    intent.requested(),
                    intent.mandatory(),
                )?;
                let session =
                    registry.resume_buffered_in(&parked.pending, &context.verification())?;
                replay_page(turn, session, parked.cursor.as_ref())
            });
            match result {
                Ok(Some(page)) => {
                    let _ = parked.pending.into_request().respond(StatusCode::OK, page);
                }
                Ok(None)
                    if deadline
                        .checked_sub(IDLE_MARGIN)
                        .is_some_and(|idle| now < idle) =>
                {
                    self.parked.push(parked);
                }
                // Deadline reached with nothing new: an empty page means idle.
                Ok(None) => {
                    let _ = parked
                        .pending
                        .into_request()
                        .respond(StatusCode::OK, Vec::new());
                }
                Err(error) => {
                    tracing::debug!(%error, "held Cloud poll refused");
                    refuse(parked.pending.into_request());
                }
            }
        }
    }
}
