//! Resumable state uploads on `POST /peer/v3/state`. A partial chunk is admitted
//! with its original proof, copied to disk by one blocking worker, and answered
//! with FlowCredit only after the original continuation is freshly revalidated.
//! The final range enters the same Store admission as a complete inline item.
use super::{data::frame, *};
use qos_relay::delivery_v3::{
    framing::{self, MessageType},
    state::{AdmissionOutcome, StateItemChunk},
};
use qos_session_peer::ingress::{
    ByteCredit, PendingCarrier, StageIdentity, StagingReservation, UploadChunk, UploadFailure,
    UploadStage,
};
use std::{collections::HashMap, io::Read, sync::mpsc};

/// Bounded concurrent disk work and idle file handles between chunks.
const INFLIGHT: usize = 8;
const IDLE: usize = 4;
type Key = ([u8; 32], [u8; 16]);

/// Anything other than one complete offset-zero item needs staging.
pub(super) fn partial_chunk(body: &[u8]) -> Option<StateItemChunk<'_>> {
    let (kind, payload, rest) = framing::decode_frame_prefix(body).ok()?;
    if kind != MessageType::StateItemChunk || !rest.is_empty() {
        return None;
    }
    let chunk = StateItemChunk::decode(payload).ok()?;
    (chunk.offset() != 0 || chunk.canonical_size() != chunk.bytes().len() as u64).then_some(chunk)
}

/// Worker output. Complete bytes were read back and digest-checked from the
/// durable stage; they remain inert until the Store admission turn.
enum Staged {
    Partial(UploadStage),
    Complete {
        bytes: Vec<u8>,
        identity: StageIdentity,
        reservation: StagingReservation,
        // Drops after the bytes it pays for.
        _credit: ByteCredit,
        _scratch: ByteCredit,
    },
}
type Done = (u64, std::result::Result<Staged, UploadFailure>);

struct InFlight {
    key: Key,
    session: [u8; 16],
    pending: PendingCarrier,
}
enum Prepared {
    Receipt(Vec<u8>),
    Stage {
        ticket: qos_session_peer::ingress::CarrierTicket,
        key: Key,
        session: [u8; 16],
        stage: std::result::Result<UploadStage, StagingReservation>,
        chunk: UploadChunk,
        now: u64,
    },
}

pub(super) struct Uploads {
    idle: Vec<UploadStage>,
    inflight: HashMap<u64, InFlight>,
    next: u64,
    done: mpsc::Receiver<Done>,
    sender: mpsc::Sender<Done>,
    /// Woken after each worker result is sent: the service loop then pumps.
    wake: Arc<tokio::sync::Notify>,
}
impl Uploads {
    pub(super) fn new(wake: Arc<tokio::sync::Notify>) -> Self {
        let (sender, done) = mpsc::channel();
        Self {
            idle: Vec::new(),
            inflight: HashMap::new(),
            next: 0,
            done,
            sender,
            wake,
        }
    }
    pub(super) fn busy(&self) -> bool {
        !self.inflight.is_empty()
    }
    fn take_idle(&mut self, identity: &StageIdentity) -> Option<UploadStage> {
        let index = self.idle.iter().position(|s| s.identity() == identity)?;
        Some(self.idle.remove(index))
    }
    fn keep_idle(&mut self, stage: UploadStage) {
        if self.idle.len() >= IDLE {
            // Dropping closes the file and releases only this lease; the
            // durable prefix remains for a later reopen.
            self.idle.remove(0);
        }
        self.idle.push(stage);
    }
    pub(super) fn fence(&mut self, session: [u8; 16]) {
        let fenced = self
            .inflight
            .iter()
            .filter(|(_, flight)| flight.session == session)
            .map(|(token, _)| *token)
            .collect::<Vec<_>>();
        for token in fenced {
            if let Some(flight) = self.inflight.remove(&token) {
                refuse(flight.pending.into_request());
            }
        }
    }
    pub(super) fn refuse_all(&mut self) {
        for (_, flight) in self.inflight.drain() {
            refuse(flight.pending.into_request());
        }
    }

    /// Admit one partial chunk. A durable receipt for the same item answers at
    /// once (the lost-final-reply probe); otherwise disk work is handed off and
    /// the original request is parked on its continuation.
    pub(super) fn start(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        work: &HostWorkInventory,
        registry: &mut Registries,
        pins: &Pins,
        request: BufferedRequest,
    ) {
        let prepared = match self.prepare(source, pool, work, registry, pins, &request) {
            Ok(prepared) => prepared,
            Err(error) => {
                tracing::debug!(%error, "resumable upload chunk refused");
                return refuse_error(request, &error);
            }
        };
        let (ticket, key, session, stage, chunk, now) = match prepared {
            Prepared::Receipt(body) => {
                let _ = request.respond(StatusCode::OK, body);
                return;
            }
            Prepared::Stage {
                ticket,
                key,
                session,
                stage,
                chunk,
                now,
            } => (ticket, key, session, stage, chunk, now),
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return refuse(request);
        };
        // A replaced original is dropped by park, which rejects it.
        let Ok(pending) = ticket.park(request) else {
            return;
        };
        let token = self.next;
        self.next = self.next.wrapping_add(1);
        self.inflight.insert(
            token,
            InFlight {
                key,
                session,
                pending,
            },
        );
        let sender = self.sender.clone();
        let wake = self.wake.clone();
        let pool = pool.clone();
        runtime.spawn_blocking(move || {
            let result = match stage {
                Ok(stage) => Ok(stage),
                Err(reservation) => UploadStage::open(reservation, &pool, now),
            }
            .and_then(|stage| stage.append(chunk, now))
            .and_then(|stage| finish_stage(stage, &pool));
            // Send, then wake: the result is in the channel before the
            // service can observe the wake-up. A wake-up with no waiter is
            // kept as the Notify permit, so none is lost.
            let _ = sender.send((token, result));
            wake.notify_one();
        });
    }

    fn prepare(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        work: &HostWorkInventory,
        registry: &mut Registries,
        pins: &Pins,
        request: &BufferedRequest,
    ) -> Result<Prepared> {
        let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
        let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
        let chunk = partial_chunk(exact.body()).context("not a partial state chunk")?;
        ensure!(self.inflight.len() < INFLIGHT, "upload workers busy");
        with_turn(source, pool, DeliveryOperation::ReceiptLookup, |turn| {
            let intent = registry.session_intent(proof.session_id())?;
            let context = turn.carrier_context(
                pins,
                intent.device(),
                intent.requested(),
                intent.mandatory(),
            )?;
            let (admitted, ticket) =
                registry.admit_buffered_in(&proof, &exact, request, &context.verification())?;
            let session = admitted.session();
            if session.capabilities() & qos_relay::delivery_v3::capabilities::RESUMABLE_STATE_V1
                == 0
            {
                bail!("resumable state not negotiated");
            }
            if let Some(admission) = turn
                .state_receipt(session, chunk.delivery(), chunk.input_digest())
                .map_err(delivery)?
            {
                return Ok(Prepared::Receipt(frame(
                    MessageType::StateAdmission,
                    &admission.encode()?,
                )?));
            }
            let identity = StageIdentity {
                workspace: turn.workspace(),
                device: session.device_certificate().device_public_key,
                delivery: chunk.delivery(),
                digest: chunk.input_digest(),
                total: chunk.canonical_size(),
            };
            let key = (identity.device, identity.delivery);
            ensure!(
                !self.inflight.values().any(|flight| flight.key == key),
                "upload chunk already in flight"
            );
            let copied = UploadChunk::copy(&chunk, pool)?;
            let stage = match self.take_idle(&identity) {
                Some(stage) => Ok(stage),
                // Staging is opened relative to the Store directory while this
                // turn holds the original Store lock.
                None => Err(work
                    .staging_directory(turn.directory(), identity.workspace)
                    .map_err(|error| anyhow!("staging unavailable: {error:?}"))?
                    .reserve(identity)
                    .map_err(|error| anyhow!("staging reservation refused: {error:?}"))?),
            };
            Ok(Prepared::Stage {
                ticket,
                key,
                session: session.session_id(),
                stage,
                chunk: copied,
                now: turn.fresh_now_ns()?,
            })
        })
    }

    /// Finish completed disk work against fresh turns. Returns whether a
    /// Store commit may have happened, so the caller wakes held polls.
    pub(super) fn pump(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        registry: &mut Registries,
        pins: &Pins,
    ) -> bool {
        let mut committed = false;
        while let Ok((token, result)) = self.done.try_recv() {
            let Some(flight) = self.inflight.remove(&token) else {
                continue;
            };
            let staged = match result {
                Ok(staged) => staged,
                Err(error) => {
                    tracing::debug!(%error, "resumable upload chunk refused");
                    // Pool pressure while staging is transient: answer the
                    // retryable 503, never a session-ending denial.
                    if matches!(error, UploadFailure::Capacity) {
                        let _ = flight.pending.into_request().respond(
                            StatusCode::SERVICE_UNAVAILABLE,
                            b"temporarily-unavailable".to_vec(),
                        );
                    } else {
                        refuse(flight.pending.into_request());
                    }
                    continue;
                }
            };
            let outcome = self.finish(source, pool, registry, pins, &flight.pending, staged);
            match outcome {
                Ok((body, durable)) => {
                    committed |= durable;
                    let _ = flight.pending.into_request().respond(StatusCode::OK, body);
                }
                Err(error) => {
                    tracing::debug!(%error, "resumable upload completion refused");
                    refuse_error(flight.pending.into_request(), &error);
                }
            }
        }
        committed
    }

    fn finish(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        registry: &mut Registries,
        pins: &Pins,
        pending: &PendingCarrier,
        staged: Staged,
    ) -> Result<(Vec<u8>, bool)> {
        match staged {
            Staged::Partial(stage) => {
                // Staging credit is disclosed only to the same still-current session.
                with_turn(source, pool, DeliveryOperation::ReceiptLookup, |turn| {
                    let intent = registry.session_intent(pending.session_id())?;
                    let context = turn.carrier_context(
                        pins,
                        intent.device(),
                        intent.requested(),
                        intent.mandatory(),
                    )?;
                    registry.resume_buffered_in(pending, &context.verification())?;
                    Ok(())
                })?;
                let progress = stage.progress()?;
                self.keep_idle(stage);
                Ok((frame(MessageType::FlowCredit, &progress.encode()?)?, false))
            }
            Staged::Complete {
                bytes,
                identity,
                reservation,
                ..
            } => {
                let admission = with_state_turn(source, pool, bytes.len(), |turn| {
                    let intent = registry.session_intent(pending.session_id())?;
                    let context = turn.carrier_context(
                        pins,
                        intent.device(),
                        intent.requested(),
                        intent.mandatory(),
                    )?;
                    let session = registry.resume_buffered_in(pending, &context.verification())?;
                    drop(context);
                    ensure!(
                        session.device_certificate().device_public_key == identity.device,
                        "staged item names another device"
                    );
                    turn.admit_state_item(session, identity.delivery, &bytes)
                        .map_err(delivery)
                })?;
                let durable = matches!(
                    admission.outcome(),
                    AdmissionOutcome::Accepted { .. } | AdmissionOutcome::Duplicate { .. }
                );
                if durable {
                    // The durable receipt now answers any retry; release disk quota.
                    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                        runtime.spawn_blocking(move || {
                            if let Err(error) = reservation.remove() {
                                tracing::debug!(?error, "admitted upload staging cleanup deferred");
                            }
                        });
                    }
                }
                Ok((
                    frame(MessageType::StateAdmission, &admission.encode()?)?,
                    durable,
                ))
            }
        }
    }
}

/// Blocking worker only: complete stages are read back and digest-checked.
fn finish_stage(
    stage: UploadStage,
    pool: &HostBytes,
) -> std::result::Result<Staged, UploadFailure> {
    let progress = stage.progress()?;
    if progress.next_offset() < progress.canonical_size() {
        return Ok(Staged::Partial(stage));
    }
    let completed = stage.complete()?;
    let (mut file, identity, reservation, scratch) = completed.into_parts();
    let total = usize::try_from(identity.total).map_err(|_| UploadFailure::Capacity)?;
    let credit = pool
        .try_acquire_bulk(total)
        .ok_or(UploadFailure::Capacity)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(total)
        .map_err(|_| UploadFailure::Capacity)?;
    (&mut file)
        .take(identity.total + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UploadFailure::Storage)?;
    if bytes.len() != total || *blake3::hash(&bytes).as_bytes() != identity.digest {
        return Err(UploadFailure::Corrupt);
    }
    Ok(Staged::Complete {
        bytes,
        identity,
        reservation,
        _credit: credit,
        _scratch: scratch,
    })
}
