//! Pinned snapshot transfers over the durable Cloud pin lifecycle:
//! select → prepare → draft → persist → commit → finalize, each owner phase in
//! a fresh reserved turn. A restart recovers a live pin from its signed sidecar
//! on the next Renew; anything unrecoverable answers SnapshotRequired.
use super::{data::frame, *};
use crate::https_owner::snapshot::{self as phases, LiveSnapshot, Renewal};
use qos_relay::delivery_v3::{
    auth::VerifiedSession,
    commit::Digest,
    framing::{self, MessageType},
    state::{ResetReason, SnapshotOperation, SnapshotRequest, SnapshotRequired},
};

/// Live pins across all devices, and per device.
const LIVE: usize = 8;
const LIVE_DEVICE: usize = 2;
/// Lock patience of a transfer's later phases (about 100 ms), once its first
/// turn was admitted: failing a later phase on a briefly held Store answers
/// 503 after effects, and the client restarts the whole transfer (a new pin
/// and several more turns). Only the first turn defers without effect.
const PHASE_RETRIES: u32 = 50;
/// Maintenance sweeps expired durable pins at most this often.
const SWEEP_NS: u64 = 60_000_000_000;

struct Transfer {
    device: [u8; 32],
    live: LiveSnapshot,
}
pub(super) struct Snapshots {
    live: Vec<Transfer>,
    swept_at: u64,
}

fn reset_reason(error: &anyhow::Error) -> Option<ResetReason> {
    match error.downcast_ref::<CloudDeliveryError>() {
        Some(CloudDeliveryError::Reset(reason)) => Some(*reason),
        _ => None,
    }
}
/// Exact `/peer/v3/snapshots/{pin}/chunks/{index}`.
fn chunk_target(path: &str) -> Option<([u8; 16], u32)> {
    let (pin, index) = path
        .strip_prefix("/peer/v3/snapshots/")?
        .split_once("/chunks/")?;
    if pin.len() != 32
        || !pin
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || index.is_empty()
        || index.len() > 4
        || (index.len() > 1 && index.starts_with('0'))
        || !index.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let index: u32 = index.parse().ok()?;
    if index >= qos_relay::delivery_v3::state::MAX_SNAPSHOT_CHUNKS {
        return None;
    }
    Some((hex::decode(pin).ok()?.try_into().ok()?, index))
}
/// Before a device's new transfer takes a durable pin and a sidecar slot (in
/// the same turn), release pins no live transfer serves (see
/// `release_orphan_snapshot_pins`) and unlink sidecars without a ledger pin.
/// Best effort: a refusal leaves them to expire and the periodic sweep.
fn release_orphans(
    turn: &mut CloudStoreTurn,
    work: &HostWorkInventory,
    device: [u8; 32],
    live: &[[u8; 16]],
) {
    let released = match turn.release_orphan_snapshot_pins(device, live) {
        Ok(released) => released.len(),
        Err(error) => {
            tracing::debug!(?error, "orphan snapshot pin release deferred");
            0
        }
    };
    // The pin removal barrier (above) precedes any physical unlink.
    let unlinked = phases::remove_unpinned_sidecars(turn, work);
    if released + unlinked > 0 {
        tracing::info!(
            event = "cloud_snapshot_orphans_released",
            released,
            unlinked,
            "Released abandoned snapshot pins before a new transfer"
        );
    }
}
/// Re-derive the same live session in a later turn; never a fresh admission.
fn revalidate<'r>(
    registry: &'r mut Registries,
    pins: &Pins,
    turn: &CloudStoreTurn,
    session: [u8; 16],
) -> Result<&'r VerifiedSession> {
    let intent = registry.session_intent(session)?;
    let context = turn.carrier_context(
        pins,
        intent.device(),
        intent.requested(),
        intent.mandatory(),
    )?;
    registry.revalidate_in(session, &context.verification())
}
/// Admit the original request proof exactly once; later phases revalidate.
fn admit(
    registry: &mut Registries,
    pins: &Pins,
    turn: &CloudStoreTurn,
    request: &BufferedRequest,
) -> Result<([u8; 16], [u8; 32])> {
    let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
    let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
    let intent = registry.session_intent(proof.session_id())?;
    let context = turn.carrier_context(
        pins,
        intent.device(),
        intent.requested(),
        intent.mandatory(),
    )?;
    let admitted = registry.admit_in(&proof, &exact, &context.verification())?;
    Ok((
        admitted.session().session_id(),
        admitted.session().device_certificate().device_public_key,
    ))
}

impl Snapshots {
    pub(super) fn new() -> Self {
        Self {
            live: Vec::new(),
            swept_at: 0,
        }
    }
    fn required(reason: ResetReason, store_generation: [u8; 16]) -> Result<Vec<u8>> {
        frame(
            MessageType::SnapshotRequired,
            &SnapshotRequired::new(reason, store_generation).encode()?,
        )
    }
    fn store_generation(source: &CloudStoreSource, pool: &HostBytes) -> Result<[u8; 16]> {
        with_turn(source, pool, DeliveryOperation::Replay, |turn| {
            Ok(turn.delivery_ledger().store_generation)
        })
    }

    /// Release expired durable pins and their signed sidecars. Called before
    /// Ready and then periodically; live transfers past expiry are dropped.
    pub(super) fn recover(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        work: &HostWorkInventory,
    ) -> Result<()> {
        self.recover_waiting(source, pool, work, BUSY_INLINE)
    }
    /// `recover` with an explicit lock patience (installation waits longer).
    pub(super) fn recover_waiting(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        work: &HostWorkInventory,
        retries: u32,
    ) -> Result<()> {
        let now = qos_relay::clock::now_ns();
        self.swept_at = now;
        self.live
            .retain(|transfer| transfer.live.expires_at_ns() > now);
        with_turn_waiting(
            source,
            pool,
            DeliveryOperation::Maintenance,
            retries,
            |turn| {
                let expired = turn.expire_snapshot_pins().map_err(delivery)?;
                phases::remove_unpinned_sidecars(turn, work);
                if expired.is_empty() {
                    return Ok(());
                }
                // The pin removal barrier precedes any physical unlink.
                let directory = work
                    .snapshot_directory(turn.directory(), turn.workspace())
                    .map_err(|error| anyhow!("snapshot storage unavailable: {error:?}"))?;
                for pin in expired {
                    if let Ok(Some(reservation)) = directory.open_existing(pin) {
                        if let Err(error) = reservation.remove() {
                            tracing::debug!(?error, "expired snapshot sidecar cleanup deferred");
                        }
                    }
                }
                Ok(())
            },
        )
    }
    pub(super) fn maintain(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        work: &HostWorkInventory,
    ) -> Result<()> {
        if qos_relay::clock::now_ns().saturating_sub(self.swept_at) < SWEEP_NS {
            return Ok(());
        }
        self.recover(source, pool, work)
    }

    /// Baseline, replay-after or renewal. Returns one framed signed manifest,
    /// or one SnapshotRequired frame when pinned history cannot be served.
    pub(super) fn request(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        work: &HostWorkInventory,
        registry: &mut Registries,
        pins: &Pins,
        request: &BufferedRequest,
    ) -> Result<Vec<u8>> {
        let (kind, payload, rest) = framing::decode_frame_prefix(request.body())
            .map_err(|_| anyhow!("invalid snapshot frame"))?;
        ensure!(
            kind == MessageType::SnapshotManifest && rest.is_empty(),
            "one snapshot request frame required"
        );
        let operation = SnapshotRequest::decode(payload)?.operation().clone();
        if let SnapshotOperation::Renew {
            pin,
            manifest_digest,
        } = operation
        {
            let (session, device) =
                with_turn(source, pool, DeliveryOperation::SnapshotSelect, |turn| {
                    admit(registry, pins, turn, request)
                })
                .context("snapshot renew admission")?;
            return match self.renew(
                source,
                pool,
                work,
                registry,
                pins,
                (session, device),
                pin,
                manifest_digest,
            ) {
                Ok(manifest) => frame(MessageType::SnapshotManifest, &manifest),
                Err(error) => {
                    let reason = reset_reason(&error).unwrap_or(ResetReason::HistoryUnavailable);
                    tracing::debug!(%error, "snapshot renewal answered SnapshotRequired");
                    Self::required(reason, Self::store_generation(source, pool)?)
                }
            };
        }
        let selected = with_turn(source, pool, DeliveryOperation::SnapshotSelect, |turn| {
            let (session, device) = admit(registry, pins, turn, request)?;
            let generation = turn.delivery_ledger().store_generation;
            let current = revalidate(registry, pins, turn, session)?;
            Ok((
                phases::select(source, turn, current, &operation).map_err(delivery),
                (session, device),
                generation,
            ))
        })
        .context("snapshot selection turn")?;
        let (selected, (session, device)) = match selected {
            (Ok(selected), identity, _) => (selected, identity),
            (Err(error), _, generation) => {
                let reason = reset_reason(&error).context("snapshot selection refused")?;
                return Self::required(reason, generation);
            }
        };
        let live_pins = self.live_pins();
        // Worker phase: original backings and credit only, no live turn.
        let prepared = selected
            .prepare()
            .map_err(delivery)
            .context("snapshot prepare")?;
        let draft = with_turn_waiting(
            source,
            pool,
            DeliveryOperation::SnapshotCommit,
            PHASE_RETRIES,
            |turn| {
                let session = revalidate(registry, pins, turn, session)?;
                release_orphans(turn, work, device, &live_pins);
                prepared.draft(turn, session, work).map_err(delivery)
            },
        )
        .context("snapshot draft")?;
        let pending = draft
            .persist()
            .map_err(delivery)
            .context("snapshot persist")?;
        let committed = with_turn_waiting(
            source,
            pool,
            DeliveryOperation::SnapshotCommit,
            PHASE_RETRIES,
            |turn| {
                let session = revalidate(registry, pins, turn, session)?;
                pending.commit(turn, session).map_err(delivery)
            },
        )
        .context("snapshot commit")?;
        let live = committed
            .finalize()
            .map_err(delivery)
            .context("snapshot finalize")?;
        let manifest = with_turn_waiting(
            source,
            pool,
            DeliveryOperation::SnapshotSelect,
            PHASE_RETRIES,
            |turn| {
                let session = revalidate(registry, pins, turn, session)?;
                Ok(live.manifest(turn, session).map_err(delivery)?.to_vec())
            },
        )?;
        self.retain(source, pool, registry, pins, session, device, live)?;
        frame(MessageType::SnapshotManifest, &manifest)
    }

    /// Pins of this process's live transfers (expired ones dropped first).
    fn live_pins(&mut self) -> Vec<[u8; 16]> {
        let now = qos_relay::clock::now_ns();
        self.live
            .retain(|transfer| transfer.live.expires_at_ns() > now);
        self.live
            .iter()
            .map(|transfer| transfer.live.pin())
            .collect()
    }

    /// Keep the new live pin, superseding the device's oldest beyond its bound.
    fn retain(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        registry: &mut Registries,
        pins: &Pins,
        session: [u8; 16],
        device: [u8; 32],
        live: LiveSnapshot,
    ) -> Result<()> {
        let now = qos_relay::clock::now_ns();
        self.live
            .retain(|transfer| transfer.live.expires_at_ns() > now);
        while self.live.iter().filter(|t| t.device == device).count() >= LIVE_DEVICE {
            let oldest = self
                .live
                .iter()
                .position(|t| t.device == device)
                .expect("counted device pin");
            let old = self.live.remove(oldest).live;
            let released = with_turn_waiting(
                source,
                pool,
                DeliveryOperation::SnapshotRelease,
                PHASE_RETRIES,
                |turn| {
                    let session = revalidate(registry, pins, turn, session)?;
                    old.release(turn, session).map_err(delivery)
                },
            );
            match released {
                Ok(released) => {
                    if let Err(error) = released.cleanup() {
                        tracing::debug!(?error, "released snapshot cleanup deferred");
                    }
                }
                // The durable pin expires on its own and the sweep reclaims it.
                Err(error) => tracing::debug!(%error, "superseded snapshot release deferred"),
            }
        }
        ensure!(self.live.len() < LIVE, "snapshot pin table full");
        self.live.push(Transfer { device, live });
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "explicit protocol inputs stay separate parameters"
    )]
    fn renew(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        work: &HostWorkInventory,
        registry: &mut Registries,
        pins: &Pins,
        (session, device): ([u8; 16], [u8; 32]),
        pin: [u8; 16],
        digest: Digest,
    ) -> Result<Vec<u8>> {
        let live = match self
            .live
            .iter()
            .position(|t| t.device == device && t.live.pin() == pin)
        {
            Some(index) => self.live.remove(index).live,
            None => Self::recover_live(source, pool, work, registry, pins, session, pin, digest)?,
        };
        let live = if live.renewable() {
            match with_turn(source, pool, DeliveryOperation::SnapshotRenew, |turn| {
                let session = revalidate(registry, pins, turn, session)?;
                live.renew(turn, session, digest).map_err(delivery)
            })? {
                Renewal::Unchanged(live) => live,
                Renewal::Pending(draft) => {
                    let pending = draft.persist().map_err(delivery)?;
                    let committed = with_turn_waiting(
                        source,
                        pool,
                        DeliveryOperation::SnapshotRenew,
                        PHASE_RETRIES,
                        |turn| {
                            let session = revalidate(registry, pins, turn, session)?;
                            pending.commit(turn, session).map_err(delivery)
                        },
                    )?;
                    committed.finalize().map_err(delivery)?
                }
            }
        } else {
            // No served progress since the last renewal: the current signed
            // manifest (or its retained previous anchor) is the answer.
            ensure!(live.matches_digest(&digest), "renewal digest mismatch");
            live
        };
        let manifest = with_turn_waiting(
            source,
            pool,
            DeliveryOperation::SnapshotSelect,
            PHASE_RETRIES,
            |turn| {
                let session = revalidate(registry, pins, turn, session)?;
                Ok(live.manifest(turn, session).map_err(delivery)?.to_vec())
            },
        )?;
        self.live.push(Transfer { device, live });
        Ok(manifest)
    }

    /// Reconcile a durable pin from its signed sidecar after a restart.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit protocol inputs stay separate parameters"
    )]
    fn recover_live(
        source: &CloudStoreSource,
        pool: &HostBytes,
        work: &HostWorkInventory,
        registry: &mut Registries,
        pins: &Pins,
        session: [u8; 16],
        pin: [u8; 16],
        digest: Digest,
    ) -> Result<LiveSnapshot> {
        let job = with_turn(source, pool, DeliveryOperation::SnapshotSelect, |turn| {
            let session = revalidate(registry, pins, turn, session)?;
            phases::open_recovery(source, turn, session, work, pin).map_err(delivery)
        })?;
        let read = job.read().map_err(delivery)?;
        let selected = with_turn_waiting(
            source,
            pool,
            DeliveryOperation::SnapshotSelect,
            PHASE_RETRIES,
            |turn| {
                let session = revalidate(registry, pins, turn, session)?;
                read.select(turn, session, digest).map_err(delivery)
            },
        )?;
        let prepared = selected.prepare().map_err(delivery)?;
        let ready = with_turn_waiting(
            source,
            pool,
            DeliveryOperation::SnapshotSelect,
            PHASE_RETRIES,
            |turn| {
                let session = revalidate(registry, pins, turn, session)?;
                prepared.reconcile(turn, session).map_err(delivery)
            },
        )?;
        ready.finish().map_err(delivery)
    }

    /// One exact chunk; the final chunk also carries the signed boundary.
    pub(super) fn chunk(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        registry: &mut Registries,
        pins: &Pins,
        request: &BufferedRequest,
    ) -> Result<Vec<u8>> {
        let (pin, index) = chunk_target(request.target()).context("invalid chunk target")?;
        ensure!(request.body().is_empty(), "chunk read carries no body");
        let (session, device) =
            with_turn(source, pool, DeliveryOperation::SnapshotSelect, |turn| {
                admit(registry, pins, turn, request)
            })?;
        let position = self
            .live
            .iter()
            .position(|t| t.device == device && t.live.pin() == pin)
            .context("unknown or expired snapshot pin")?;
        let live = self.live.remove(position).live;
        let digest = match live.current_digest() {
            Ok(digest) => digest,
            Err(error) => {
                self.live.push(Transfer { device, live });
                return Err(delivery(error));
            }
        };
        let prepared = match live.prepare_chunk(index, digest) {
            Ok(prepared) => prepared,
            Err(refusal) => {
                let (live, error) = refusal.into_parts();
                self.live.push(Transfer { device, live });
                return Err(delivery(error));
            }
        };
        let (live, reply) = with_turn_waiting(
            source,
            pool,
            DeliveryOperation::SnapshotSelect,
            PHASE_RETRIES,
            |turn| {
                let session = revalidate(registry, pins, turn, session)?;
                prepared.authorize(turn, session).map_err(delivery)
            },
        )?;
        let bytes = reply.bytes().to_vec();
        self.live.push(Transfer { device, live });
        Ok(bytes)
    }
}
