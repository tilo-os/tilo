//! Resumable state uploads on `POST /peer/v3/state`. A partial chunk is admitted
//! with its original proof, copied to disk by one blocking worker, and answered
//! with FlowCredit only after the original continuation is freshly revalidated.
//! The final range enters native admission in a genuine staged owner turn.
use super::*;
use crate::{
    delivery_http::{
        revalidation::{PendingOperation, continuation::AdmissionContinuation},
        upload::{UploadChunk, UploadFailure, UploadStage},
    },
    staging_manifest::StageIdentity,
    store::HostWorkInventory,
};
use std::{collections::HashMap, sync::mpsc};
use tilo_relay::delivery_v3::{
    framing::{self, MessageType},
    state::StateItemChunk,
};

/// Bounded concurrent disk work and idle file handles between chunks.
const INFLIGHT: usize = 8;
const IDLE: usize = 4;
/// Inactivity after which a stage without committed metadata is abandoned
/// (the committed-metadata expiry uses the same five-minute window).
const ABANDONED_NS: u64 = 300_000_000_000;
type Key = ([u8; 32], [u8; 16]);
type Done = (u64, std::result::Result<UploadStage, UploadFailure>);

struct InFlight {
    key: Key,
    pending: PendingOperation,
}

pub(super) struct Uploads {
    work: HostWorkInventory,
    /// Oldest first. Each keeps its reservation lease and scratch credit.
    idle: Vec<UploadStage>,
    inflight: HashMap<u64, InFlight>,
    next: u64,
    done: mpsc::Receiver<Done>,
    sender: mpsc::Sender<Done>,
}

/// Whether the single framed chunk needs the staged path: anything other than
/// a complete offset-zero item. Complete items keep the inline batch path.
pub(super) fn partial_chunk(body: &[u8]) -> Option<StateItemChunk<'_>> {
    let (kind, payload, rest) = framing::decode_frame_prefix(body).ok()?;
    if kind != MessageType::StateItemChunk || !rest.is_empty() {
        return None;
    }
    let chunk = StateItemChunk::decode(payload).ok()?;
    (chunk.offset() != 0 || chunk.canonical_size() != chunk.bytes().len() as u64).then_some(chunk)
}

fn frame(kind: MessageType, payload: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    framing::append_frame(&mut out, kind, payload, 1024 * 1024)?;
    Ok(out)
}

fn at(now: u64) -> web_time::SystemTime {
    web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now)
}

/// Result of the synchronous admission step.
enum Prepared {
    Receipt(Vec<u8>),
    Stage {
        ticket: AdmissionContinuation,
        key: Key,
        stage: std::result::Result<UploadStage, crate::store::StagingReservation>,
        chunk: UploadChunk,
        now: u64,
    },
}

impl Uploads {
    pub(super) fn new(work: HostWorkInventory) -> Self {
        let (sender, done) = mpsc::channel();
        Self {
            work,
            idle: Vec::new(),
            inflight: HashMap::new(),
            next: 0,
            done,
            sender,
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

    /// Admit one partial chunk. A durable receipt for the same item answers at
    /// once (the lost-final-reply probe); otherwise disk work is handed off and
    /// the original request is parked on its continuation.
    pub(super) fn start(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &mut SessionOwner,
        budget: &HostBytes,
        request: BufferedRequest,
    ) -> std::result::Result<(), BufferedRequest> {
        let prepared = match self.prepare(sessions, current, owner, budget, &request) {
            Ok(prepared) => prepared,
            Err(error) if super::transient_capacity(&error) => {
                tracing::debug!(%error, "resumable upload chunk deferred");
                super::refuse_error(request, &error);
                return Ok(());
            }
            Err(error) => {
                tracing::debug!(%error, "resumable upload chunk refused");
                return Err(request);
            }
        };
        let (ticket, key, stage, chunk, now) = match prepared {
            Prepared::Receipt(body) => {
                let _ = request.respond(StatusCode::OK, body);
                return Ok(());
            }
            Prepared::Stage {
                ticket,
                key,
                stage,
                chunk,
                now,
            } => (ticket, key, stage, chunk, now),
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return Err(request);
        };
        // A replaced original is dropped by park, which rejects it.
        let Ok(pending) = ticket.park(request) else {
            return Ok(());
        };
        let token = self.next;
        self.next = self.next.wrapping_add(1);
        self.inflight.insert(token, InFlight { key, pending });
        let sender = self.sender.clone();
        let pool = budget.clone();
        runtime.spawn_blocking(move || {
            let result = match stage {
                Ok(stage) => Ok(stage),
                Err(reservation) => UploadStage::open(reservation, &pool, now),
            }
            .and_then(|stage| stage.append(chunk, now));
            let _ = sender.send((token, result));
        });
        Ok(())
    }

    fn prepare(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &mut SessionOwner,
        budget: &HostBytes,
        request: &BufferedRequest,
    ) -> Result<Prepared> {
        let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
        let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
        let chunk = partial_chunk(exact.body()).context("not a partial state chunk")?;
        let (admitted, mut checker, mut turn) = sessions.admit_revalidating(
            owner,
            current,
            budget,
            OwnerDataOperation::ReceiptLookup,
            &proof,
            &exact,
            request,
        )?;
        let guard = current.read().map_err(anyhow::Error::msg)?;
        let now = checker.check(owner, guard.view(), admitted.session())?;
        let source = owner.data_source(&mut turn, guard.view(), admitted.session(), now)?;
        let receipt = owner
            .data_admission_receipt(&mut turn, &source, chunk.delivery(), chunk.input_digest())?
            .map(|record| cores::receipt_admission(chunk.delivery(), chunk.input_digest(), record))
            .transpose()?;
        if let Some(admission) = receipt {
            return Ok(Prepared::Receipt(frame(
                MessageType::StateAdmission,
                &admission.encode()?,
            )?));
        }
        ensure!(self.inflight.len() < INFLIGHT, "upload workers busy");
        let identity = StageIdentity {
            workspace: owner.workspace_id(),
            device: admitted.session().device_certificate().device_public_key,
            delivery: chunk.delivery(),
            digest: chunk.input_digest(),
            total: chunk.canonical_size(),
        };
        let key = (identity.device, identity.delivery);
        ensure!(
            !self.inflight.values().any(|flight| flight.key == key),
            "upload chunk already in flight"
        );
        let copied = UploadChunk::copy(&chunk, budget)?;
        let stage = match self.take_idle(&identity) {
            Some(stage) => Ok(stage),
            None => {
                // A new stage first releases this device's abandoned or already
                // admitted staging, so neither blocks its two-stage bound.
                self.reclaim(owner, &mut turn, &source, identity.device, now);
                Err(owner.reserve_upload_staging(&self.work, identity)?)
            }
        };
        drop(source);
        drop(guard);
        let ticket = checker.capture_continuation(&admitted, request)?;
        Ok(Prepared::Stage {
            ticket,
            key,
            stage,
            chunk: copied,
            now,
        })
    }

    /// Remove idle staging (no worker, file or handle lease) of one device
    /// that is either already admitted (its durable receipt answers every
    /// retry) or abandoned (committed metadata expired, or no metadata and no
    /// write within the idle window). Unreadable staging is kept: fail closed.
    fn reclaim(
        &mut self,
        owner: &SessionOwner,
        turn: &mut crate::owner::resources::OwnerResourceTurn,
        source: &crate::owner::delivery::consumer::VerifiedDeliverySource<'_>,
        device: [u8; 32],
        now: u64,
    ) -> usize {
        let Ok(stages) = owner.idle_upload_stages(&self.work, device) else {
            return 0;
        };
        let mut reclaimed = 0;
        for stage in stages {
            let identity = *stage.identity();
            let admitted = owner
                .data_admission_receipt(turn, source, identity.delivery, identity.digest)
                .is_ok_and(|receipt| receipt.is_some());
            let abandoned = match stage.read_manifest() {
                Ok(Some(manifest)) => manifest.expires_at_ns <= now,
                Ok(None) => stage
                    .open_data(false)
                    .ok()
                    .and_then(|file| file.metadata().ok())
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                    .is_none_or(|modified| {
                        now.saturating_sub(modified.as_nanos() as u64) >= ABANDONED_NS
                    }),
                Err(_) => false,
            };
            if admitted || abandoned {
                match stage.remove() {
                    Ok(()) => reclaimed += 1,
                    Err(error) => tracing::debug!(?error, "staging reclamation deferred"),
                }
            }
        }
        reclaimed
    }

    /// Finish completed disk work against the current owner. Returns whether
    /// a native commit may have happened.
    pub(super) fn pump(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &mut SessionOwner,
        budget: &HostBytes,
    ) -> bool {
        let mut committed = false;
        while let Ok((token, result)) = self.done.try_recv() {
            let Some(flight) = self.inflight.remove(&token) else {
                continue;
            };
            let stage = match result {
                Ok(stage) => stage,
                Err(UploadFailure::Capacity) => {
                    tracing::debug!("resumable upload chunk deferred by capacity");
                    let _ = flight.pending.into_request().respond(
                        StatusCode::SERVICE_UNAVAILABLE,
                        b"temporarily-unavailable".to_vec(),
                    );
                    continue;
                }
                Err(error) => {
                    tracing::debug!(%error, "resumable upload chunk refused");
                    refuse(flight.pending.into_request());
                    continue;
                }
            };
            let outcome = self.finish(sessions, current, owner, budget, &flight.pending, stage);
            committed |= matches!(outcome, Ok((_, true)));
            match outcome {
                Ok((body, _)) => {
                    let _ = flight.pending.into_request().respond(StatusCode::OK, body);
                }
                Err(error) => {
                    tracing::debug!(%error, "resumable upload completion refused");
                    super::refuse_error(flight.pending.into_request(), &error);
                }
            }
        }
        committed
    }

    fn finish(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &mut SessionOwner,
        budget: &HostBytes,
        pending: &PendingOperation,
        stage: UploadStage,
    ) -> Result<(Vec<u8>, bool)> {
        let (session, mut checker, turn) =
            sessions.resume_operation(owner, current, budget, pending)?;
        let progress = stage.progress()?;
        if progress.next_offset() < progress.canonical_size() {
            // Staging credit is disclosed only to the same still-current session.
            let guard = current.read().map_err(anyhow::Error::msg)?;
            checker.check(owner, guard.view(), session)?;
            drop(guard);
            drop(turn);
            self.keep_idle(stage);
            return Ok((frame(MessageType::FlowCredit, &progress.encode()?)?, false));
        }
        drop(turn);
        let identity = *stage.identity();
        let completed = stage.complete()?;
        let input_len = usize::try_from(identity.total)?;
        let mut turn = owner.prepare_staged_turn(budget, &self.work, input_len)?;
        let (outcome, reservation) =
            current.with_owner_view(owner, |view, owner| -> Result<_> {
                let now = checker.check(owner, view, session)?;
                let source = owner.data_source(&mut turn, view, session, now)?;
                Ok(owner.data_admit_staged(&mut turn, &source, completed, at(now))?)
            })??;
        drop(turn);
        let durable = matches!(
            outcome,
            crate::owner::delivery::AdmissionOutcome::Durable(_)
        );
        let admission = cores::admission_for(identity.delivery, identity.digest, outcome)?;
        if durable {
            // The durable receipt now answers any retry; release the disk quota.
            // A failed removal leaves an idle stage whose receipt exists; the
            // device's next new upload (or a restart) reclaims it.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(move || {
                    if let Err(error) = reservation.remove() {
                        tracing::debug!(%error, "admitted upload staging cleanup deferred");
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
