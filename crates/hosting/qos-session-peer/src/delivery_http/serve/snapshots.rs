//! Pinned snapshot transfers: `POST /peer/v3/snapshots` selects and signs a
//! manifest over owner-pinned publication backings; chunk reads revalidate
//! the original selection against Current before any byte is disclosed.
//!
//! Each issued manifest (and its previous anchor after a renewal) is kept in
//! the store's private QSP2 sidecar for its pin. After a host restart, a
//! (reload) Renew naming that pin reconstructs the transfer from the signed
//! claims: the host signature, pin, consumer and expiry are checked, the exact
//! cut is re-selected from the currently retained history, and the rebuilt
//! stream must reproduce every signed chunk digest before anything is served.
//! History that is no longer retained, a rotated store or a changed policy
//! still answers SnapshotRequired. Expired and orphaned sidecars are swept.
use super::*;
use crate::{
    delivery_http::snapshot::{SnapshotStream, pair::ManifestPair},
    owner::delivery::{
        consumer::snapshot::SnapshotSelection, error::OwnerDataFailureKind as Failure,
    },
    store::{HostWorkInventory, SnapshotReservation},
};
use tilo_relay::delivery_v3::{
    commit::Digest,
    framing::{self, MessageType},
    state::{
        CommitBoundary, ResetReason, SNAPSHOT_CHUNK_BYTES, SnapshotChunk, SnapshotManifest,
        SnapshotOperation, SnapshotRequest, SnapshotRequired,
    },
};

/// Live pins across all devices, and per device.
const LIVE: usize = 8;
const LIVE_DEVICE: usize = 2;
/// Signed pin deadline; the client renews a few seconds before it.
const LIFETIME_NS: u64 = 240_000_000_000;
/// Earlier manifest digests a staged client may present for resumption.
const HISTORY: usize = 8;
/// Interval between sweeps of expired or orphaned sidecars.
const SWEEP_NS: u64 = 60_000_000_000;

struct Transfer {
    device: [u8; 32],
    selection: SnapshotSelection,
    stream: SnapshotStream,
    manifest: SnapshotManifest,
    expires_at_ns: u64,
    digests: Vec<Digest>,
    /// Durable signed (current, previous) pair for restart recovery.
    sidecar: Option<SnapshotReservation>,
    previous: Option<Vec<u8>>,
}
impl Transfer {
    /// Replace the durable pair; a failure leaves the transfer served from
    /// memory only (it then cannot be recovered after a restart).
    fn persist(&mut self) {
        let Some(sidecar) = &self.sidecar else { return };
        if let Err(error) = persist(sidecar, self.manifest.as_bytes(), self.previous.as_deref()) {
            tracing::debug!(%error, "snapshot sidecar not persisted; pin is process-local");
        }
    }
    fn release(mut self) {
        if let Some(sidecar) = self.sidecar.take() {
            if let Err(error) = sidecar.remove() {
                tracing::debug!(?error, "snapshot sidecar cleanup deferred to the sweep");
            }
        }
    }
}
fn persist(sidecar: &SnapshotReservation, current: &[u8], previous: Option<&[u8]>) -> Result<()> {
    let pair = ManifestPair::new(current, previous)
        .map_err(|error| anyhow::anyhow!("snapshot pair: {error:?}"))?;
    let mut bytes = vec![0; pair.encoded_len()];
    pair.write_to(&mut bytes)
        .map_err(|error| anyhow::anyhow!("snapshot pair: {error:?}"))?;
    let host =
        |error: crate::store::HostWorkFailure| anyhow::anyhow!("snapshot sidecar: {error:?}");
    // An earlier interrupted write leaves an inert candidate; this process
    // owns the only lease, so it is replaced rather than promoted.
    if sidecar.read_pending().map_err(host)?.is_some() {
        sidecar.discard_pending().map_err(host)?;
    }
    sidecar.prepare(&bytes).map_err(host)?;
    sidecar.finalize().map_err(host)
}

pub(super) struct Snapshots {
    live: Vec<Transfer>,
    work: HostWorkInventory,
    swept_at: u64,
}

fn frame(out: &mut Vec<u8>, kind: MessageType, payload: &[u8]) -> Result<()> {
    framing::append_frame(out, kind, payload, 1024 * 1024)?;
    Ok(())
}

pub(super) fn reset_reason(error: &anyhow::Error) -> Option<ResetReason> {
    error
        .downcast_ref::<crate::owner::delivery::error::OwnerDataFailure>()
        .and_then(|failure| match failure.kind() {
            Failure::Reset(reason) => Some(reason),
            _ => None,
        })
}

impl Snapshots {
    pub(super) fn new(work: HostWorkInventory) -> Self {
        Self {
            live: Vec::new(),
            work,
            swept_at: 0,
        }
    }

    fn prune(&mut self, now: u64) {
        let (expired, live) = std::mem::take(&mut self.live)
            .into_iter()
            .partition(|transfer| transfer.expires_at_ns <= now);
        self.live = live;
        for transfer in expired {
            Transfer::release(transfer);
        }
    }

    /// Remove sidecars no live transfer holds whose signed pin expired, or
    /// that hold no authentic current manifest. Unexpired ones stay
    /// recoverable by a reload Renew.
    fn sweep(&mut self, owner: &SessionOwner, now: u64) {
        if now.saturating_sub(self.swept_at) < SWEEP_NS {
            return;
        }
        self.swept_at = now;
        let Ok(directory) = owner.snapshot_directory(&self.work) else {
            return;
        };
        let Ok(key) =
            ed25519_dalek::VerifyingKey::from_bytes(&owner.host_certificate().device_public_key)
        else {
            return;
        };
        for pin in directory.idle_pins().unwrap_or_default() {
            let Ok(Some(sidecar)) = directory.open_existing(pin) else {
                continue;
            };
            let live = sidecar.read_current().ok().flatten().is_some_and(|bytes| {
                ManifestPair::decode(bytes.bytes())
                    .ok()
                    .and_then(|pair| pair.authenticate_retained(&key).ok())
                    .is_some_and(|pair| {
                        pair.current().issue().pin == pin
                            && pair.current().issue().expires_at_ns > now
                    })
            });
            if !live {
                if let Err(error) = sidecar.remove() {
                    tracing::debug!(?error, "orphan snapshot sidecar cleanup deferred");
                }
            }
        }
    }

    /// Rebuild a transfer from its durable signed pair after a restart.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit protocol inputs stay separate parameters"
    )]
    fn recover(
        &mut self,
        owner: &mut SessionOwner,
        current: &CommittedAuthority,
        budget: &HostBytes,
        checker: &mut crate::delivery_http::revalidation::OperationRevalidation<'_>,
        session: &auth::VerifiedSession,
        device: [u8; 32],
        pin: [u8; 16],
        requested: &Digest,
    ) -> Result<Option<usize>> {
        let directory = owner.snapshot_directory(&self.work)?;
        let Some(sidecar) = directory
            .open_existing(pin)
            .map_err(|error| anyhow::anyhow!("snapshot sidecar: {error:?}"))?
        else {
            return Ok(None);
        };
        let Some(bytes) = sidecar
            .read_current()
            .map_err(|error| anyhow::anyhow!("snapshot sidecar: {error:?}"))?
        else {
            return Ok(None);
        };
        let pair = ManifestPair::decode(bytes.bytes())
            .map_err(|error| anyhow::anyhow!("snapshot pair: {error:?}"))?;
        let key =
            ed25519_dalek::VerifyingKey::from_bytes(&owner.host_certificate().device_public_key)?;
        let signed = pair
            .authenticate_retained(&key)
            .map_err(|error| anyhow::anyhow!("snapshot pair: {error:?}"))?;
        let manifest = SnapshotManifest::from_bytes(pair.current())?;
        let previous = pair.previous().map(<[u8]>::to_vec);
        let mut digests = Vec::new();
        if let Some(previous) = pair.previous() {
            digests.push(SnapshotManifest::from_bytes(previous)?.digest());
        }
        digests.push(manifest.digest());
        let issue = signed.current().issue().clone();
        let mut turn = owner.prepare_data_turn(budget, OwnerDataOperation::SnapshotPrepare)?;
        let guard = current.read().map_err(anyhow::Error::msg)?;
        let now = checker.check(owner, guard.view(), session)?;
        if issue.pin != pin
            || issue.scope.consumer_device != device
            || issue.expires_at_ns <= now
            || !digests.contains(requested)
        {
            return Ok(None);
        }
        let source = owner.data_source(&mut turn, guard.view(), session, now)?;
        let (selection, custody) = owner
            .data_reselect_snapshot(&mut turn, &source, &issue, budget)
            .map_err(anyhow::Error::from)?;
        // Selection and revalidation are separate owner turns, as for a new pin.
        drop(source);
        drop(turn);
        let stream = SnapshotStream::prepare(custody.into_custody(), budget)
            .map_err(|error| anyhow::anyhow!("snapshot stream refused: {error:?}"))?;
        ensure!(
            stream.chunk_digests() == issue.chunk_digests.as_slice()
                && stream.stream_bytes() == issue.stream_bytes
                && stream.item_count() == issue.item_count as usize,
            "recovered snapshot stream differs from its signed manifest"
        );
        let mut turn = owner.prepare_data_turn(budget, OwnerDataOperation::SnapshotPrepare)?;
        let now = checker.check(owner, guard.view(), session)?;
        let source = owner.data_source(&mut turn, guard.view(), session, now)?;
        owner.data_revalidate_snapshot(&mut turn, &source, &selection, stream.custody(), budget)?;
        drop(source);
        drop(guard);
        drop(bytes);
        self.live.push(Transfer {
            device,
            selection,
            stream,
            manifest,
            expires_at_ns: issue.expires_at_ns,
            digests,
            sidecar: Some(sidecar),
            previous,
        });
        Ok(Some(self.live.len() - 1))
    }

    pub(super) fn required(owner: &SessionOwner, reason: ResetReason) -> Result<Vec<u8>> {
        let generation = owner.delivery_ledger()?.store_generation;
        let mut out = Vec::new();
        frame(
            &mut out,
            MessageType::SnapshotRequired,
            &SnapshotRequired::new(reason, generation).encode()?,
        )?;
        Ok(out)
    }

    /// Owner-loop entry for a snapshot request (see serve.rs). Answers
    /// synchronously today; the hook lets manifest hashing move off the owner
    /// loop, answering later from `pump`, without changing the dispatcher.
    pub(super) fn start(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &mut SessionOwner,
        budget: &HostBytes,
        request: BufferedRequest,
    ) -> std::result::Result<(), BufferedRequest> {
        match self.request(sessions, current, owner, budget, &request) {
            Ok(body) => {
                let _ = request.respond(hyper::StatusCode::OK, body);
                Ok(())
            }
            Err(error) => {
                tracing::debug!(route = ?request.route(), %error, "data request refused");
                Err(request)
            }
        }
    }
    /// Complete handed-off snapshot work (none yet).
    pub(super) fn pump(
        &mut self,
        _sessions: &mut HttpSessions,
        _current: &CommittedAuthority,
        _owner: &mut SessionOwner,
        _budget: &HostBytes,
    ) {
    }
    pub(super) fn busy(&self) -> bool {
        false
    }
    /// Whether any live pin was signed (issued or renewed) within `window_ns`.
    pub(super) fn recently_pinned(&self, now_ns: u64, window_ns: u64) -> bool {
        self.live.iter().any(|transfer| {
            transfer
                .expires_at_ns
                .saturating_sub(LIFETIME_NS)
                .saturating_add(window_ns)
                > now_ns
        })
    }

    /// Baseline, replay-after or renewal. Returns one framed manifest, or one
    /// SnapshotRequired frame when the pinned history can no longer be served.
    pub(super) fn request(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &mut SessionOwner,
        budget: &HostBytes,
        request: &BufferedRequest,
    ) -> Result<Vec<u8>> {
        let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
        let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
        let (kind, payload, rest) = framing::decode_frame_prefix(exact.body())?;
        ensure!(
            kind == MessageType::SnapshotManifest && rest.is_empty(),
            "one snapshot request frame required"
        );
        let operation = SnapshotRequest::decode(payload)?.operation().clone();
        let (admitted, mut checker, mut turn) = sessions.admit_revalidating(
            owner,
            current,
            budget,
            OwnerDataOperation::SnapshotPrepare,
            &proof,
            &exact,
            request,
        )?;
        let session = admitted.session();
        let device = session.device_certificate().device_public_key;
        if let SnapshotOperation::Renew {
            pin,
            manifest_digest,
        } = operation
        {
            drop(turn);
            let now = {
                let guard = current.read().map_err(anyhow::Error::msg)?;
                checker.check(owner, guard.view(), session)?
            };
            self.prune(now);
            let found = self.live.iter().position(|transfer| {
                transfer.device == device
                    && transfer.selection.pin() == pin
                    && transfer.digests.contains(&manifest_digest)
            });
            let index = match found {
                Some(index) => index,
                None if !self
                    .live
                    .iter()
                    .any(|transfer| transfer.selection.pin() == pin) =>
                {
                    match self.recover(
                        owner,
                        current,
                        budget,
                        &mut checker,
                        session,
                        device,
                        pin,
                        &manifest_digest,
                    ) {
                        Ok(Some(index)) => index,
                        Ok(None) => return Self::required(owner, ResetReason::HistoryUnavailable),
                        Err(error) => {
                            let reason =
                                reset_reason(&error).unwrap_or(ResetReason::HistoryUnavailable);
                            tracing::debug!(%error, "snapshot pin recovery answered SnapshotRequired");
                            return Self::required(owner, reason);
                        }
                    }
                }
                None => return Self::required(owner, ResetReason::HistoryUnavailable),
            };
            let mut turn = owner.prepare_data_turn(budget, OwnerDataOperation::SnapshotPrepare)?;
            let guard = current.read().map_err(anyhow::Error::msg)?;
            let now = checker.check(owner, guard.view(), session)?;
            let source = owner.data_source(&mut turn, guard.view(), session, now)?;
            let transfer = &self.live[index];
            if let Err(error) = owner
                .data_revalidate_snapshot(
                    &mut turn,
                    &source,
                    &transfer.selection,
                    transfer.stream.custody(),
                    budget,
                )
                .map_err(anyhow::Error::from)
            {
                drop(source);
                drop(guard);
                let reason = reset_reason(&error).context("snapshot renewal refused")?;
                Transfer::release(self.live.remove(index));
                return Self::required(owner, reason);
            }
            let expires = now
                .checked_add(LIFETIME_NS)
                .context("pin deadline overflow")?;
            let manifest = if expires > transfer.expires_at_ns {
                owner.sign_snapshot_manifest(&transfer.selection, &transfer.stream, expires, now)?
            } else {
                SnapshotManifest::from_bytes(transfer.manifest.as_bytes())?
            };
            drop(source);
            drop(guard);
            let transfer = &mut self.live[index];
            if manifest.digest() != transfer.manifest.digest() {
                if transfer.digests.len() >= HISTORY {
                    transfer.digests.remove(0);
                }
                transfer.digests.push(manifest.digest());
                transfer.expires_at_ns = expires;
                transfer.previous = Some(transfer.manifest.as_bytes().to_vec());
                transfer.manifest = SnapshotManifest::from_bytes(manifest.as_bytes())?;
                transfer.persist();
            }
            let mut out = Vec::new();
            frame(&mut out, MessageType::SnapshotManifest, manifest.as_bytes())?;
            return Ok(out);
        }
        let selected = current.with_owner_view(owner, |view, owner| -> Result<_> {
            let now = checker.check(owner, view, session)?;
            let source = owner.data_source(&mut turn, view, session, now)?;
            Ok((
                owner.data_select_snapshot(&mut turn, &source, &operation, budget),
                now,
            ))
        })??;
        drop(turn);
        let (selection, pin) = match selected {
            (Ok(selected), _) => selected,
            (Err(error), _) => {
                let error = anyhow::Error::from(error);
                let reason = reset_reason(&error).context("snapshot selection refused")?;
                return Self::required(owner, reason);
            }
        };
        let stream = SnapshotStream::prepare(pin.into_custody(), budget)
            .map_err(|error| anyhow::anyhow!("snapshot stream refused: {error:?}"))?;
        // Selection and signing are separate owner turns; re-check that the
        // selected history is still exactly retained before signing.
        let mut turn = owner.prepare_data_turn(budget, OwnerDataOperation::SnapshotPrepare)?;
        let guard = current.read().map_err(anyhow::Error::msg)?;
        let now = checker.check(owner, guard.view(), session)?;
        let source = owner.data_source(&mut turn, guard.view(), session, now)?;
        owner.data_revalidate_snapshot(&mut turn, &source, &selection, stream.custody(), budget)?;
        let expires = now
            .checked_add(LIFETIME_NS)
            .context("pin deadline overflow")?;
        let manifest = owner.sign_snapshot_manifest(&selection, &stream, expires, now)?;
        drop(source);
        drop(guard);
        self.prune(now);
        self.sweep(owner, now);
        if self.live.iter().filter(|t| t.device == device).count() >= LIVE_DEVICE {
            // The device's oldest pin is superseded by its newest request.
            let oldest = self
                .live
                .iter()
                .position(|t| t.device == device)
                .expect("counted device pin");
            Transfer::release(self.live.remove(oldest));
        }
        ensure!(self.live.len() < LIVE, "snapshot pin table full");
        let mut out = Vec::new();
        frame(&mut out, MessageType::SnapshotManifest, manifest.as_bytes())?;
        let sidecar = owner
            .snapshot_directory(&self.work)
            .map_err(anyhow::Error::from)
            .and_then(|directory| {
                directory
                    .reserve(selection.pin())
                    .map_err(|error| anyhow::anyhow!("snapshot sidecar: {error:?}"))
            });
        let sidecar = match sidecar {
            Ok(sidecar) => Some(sidecar),
            Err(error) => {
                tracing::debug!(%error, "snapshot sidecar unavailable; pin is process-local");
                None
            }
        };
        let mut transfer = Transfer {
            device,
            selection,
            stream,
            digests: vec![manifest.digest()],
            manifest,
            expires_at_ns: expires,
            sidecar,
            previous: None,
        };
        transfer.persist();
        self.live.push(transfer);
        Ok(out)
    }

    /// One exact 240 KiB chunk; the final chunk also carries the signed
    /// boundary for the pinned cut.
    pub(super) fn chunk(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &mut SessionOwner,
        budget: &HostBytes,
        request: &BufferedRequest,
    ) -> Result<Vec<u8>> {
        let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
        let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
        let (pin, index) =
            crate::https::snapshot_chunk_target(exact.target()).context("invalid chunk target")?;
        ensure!(exact.body().is_empty(), "chunk read carries no body");
        let (admitted, mut checker, mut turn) = sessions.admit_revalidating(
            owner,
            current,
            budget,
            OwnerDataOperation::SnapshotPrepare,
            &proof,
            &exact,
            request,
        )?;
        let session = admitted.session();
        let device = session.device_certificate().device_public_key;
        let guard = current.read().map_err(anyhow::Error::msg)?;
        let now = checker.check(owner, guard.view(), session)?;
        self.prune(now);
        let transfer = self
            .live
            .iter()
            .find(|t| t.device == device && t.selection.pin() == pin)
            .context("unknown or expired snapshot pin")?;
        let source = owner.data_source(&mut turn, guard.view(), session, now)?;
        owner.data_revalidate_snapshot(
            &mut turn,
            &source,
            &transfer.selection,
            transfer.stream.custody(),
            budget,
        )?;
        let mut scratch = vec![0; SNAPSHOT_CHUNK_BYTES];
        let length = transfer
            .stream
            .fill_chunk(index, &mut scratch)
            .map_err(|error| anyhow::anyhow!("snapshot chunk refused: {error:?}"))?;
        let digest = transfer.manifest.digest();
        let chunk = SnapshotChunk::new(pin, digest, index, &scratch[..length])?.encode()?;
        let mut out = Vec::new();
        frame(&mut out, MessageType::SnapshotChunk, &chunk)?;
        let end = u64::from(index) * SNAPSHOT_CHUNK_BYTES as u64 + length as u64;
        if end == transfer.stream.stream_bytes() {
            // The boundary cursor is issued in its own replay turn for the
            // exact pinned cut, so the client resumes replay after it.
            drop(source);
            let committed = transfer.selection.scope().committed.clone();
            let mut replay = owner.prepare_data_turn(budget, OwnerDataOperation::Replay)?;
            let source = owner.data_source(&mut replay, guard.view(), session, now)?;
            let token = owner.data_issue_cursor(&mut replay, &source, committed.clone())?;
            let scope = transfer.selection.scope();
            let boundary = CommitBoundary::new(
                scope.consumer,
                scope.authorization_view,
                scope.store_generation,
                committed,
                u32::try_from(transfer.stream.item_count())?,
                transfer.stream.visible_order(),
                token,
            )?
            .encode()?;
            frame(&mut out, MessageType::CommitBoundary, &boundary)?;
        }
        Ok(out)
    }
}
