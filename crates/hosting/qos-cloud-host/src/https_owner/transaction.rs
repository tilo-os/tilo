//! Cloud transaction entry: one host-byte reservation sized from the persisted
//! aggregate BEFORE the actual Store lock/decode, then the original Store barrier.
use super::{CloudStoreSource, CloudStoreTurn, Current};
use automerge::resource_limits::NativeWorkTurn;
use qos_relay::delivery_v3::{
    auth::{self, VerifiedSession},
    commit::{
        CommitRef, ConsumerId, ConsumerState, DeliveryLedgerV1, PublicationPool, PublicationRef,
        SnapshotPin,
    },
    cursor::{CursorIssue, CursorToken, SignedCursorClaims},
    state::{ManifestIssue, ResetReason, SNAPSHOT_CHUNK_BYTES, SnapshotKind, SnapshotManifest},
};
use qos_session_peer::ingress::ByteCredit;
use qos_session_peer::ingress::{
    AuthenticatedSnapshotPair, HostBytes, PublicationCustody, RetainedAuthenticatedSnapshotPair,
    SnapshotManifestPair, SnapshotStream,
    delivery_planning::{DeliveryPolicyBasis, PlanningFailure},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeliveryOperation {
    RegisterConsumer,
    Cursor,
    Acknowledge,
    Replay,
    SnapshotSelect,
    SnapshotCommit,
    SnapshotRenew,
    SnapshotRelease,
    /// Complete incoming state item; the input length is part of the reservation.
    State,
    /// Read-only durable receipt lookup for a lost final reply.
    ReceiptLookup,
    /// Owner-signed descriptor renewal and carrier queue maintenance.
    Maintenance,
    /// One onboarding exchange: the Store join operation and its signed reply.
    Onboarding,
}
impl DeliveryOperation {
    /// Explicit serialized admission budget for one turn: the decoded aggregate,
    /// its candidate clone, encoded output and transient copies. This is an
    /// admission policy on the shared ingress pool, not a native heap measurement.
    fn reserved_bytes(self, aggregate: usize, input: usize) -> Option<usize> {
        let base = match self {
            Self::Replay | Self::Cursor | Self::ReceiptLookup | Self::SnapshotSelect => {
                aggregate.checked_mul(3)?
            }
            Self::RegisterConsumer | Self::Acknowledge | Self::Maintenance => {
                aggregate.checked_mul(5)?.checked_add(128 * 1024)?
            }
            Self::SnapshotCommit | Self::SnapshotRenew | Self::SnapshotRelease => aggregate
                .checked_mul(5)?
                .checked_add(qos_relay::delivery_v3::commit::MAX_LEDGER_BYTES)?,
            Self::Onboarding => aggregate.checked_mul(6)?.checked_add(2 * 1024 * 1024)?,
            // The complete input itself is already paid by its carrier credit;
            // this covers its decoded message and the candidate/outbox copies.
            Self::State => aggregate
                .checked_mul(4)?
                .checked_add(input.checked_mul(2)?)?
                .checked_add(512 * 1024)?,
        };
        base.checked_add(256 * 1024)
    }
    /// The same budget when the turn reuses the retained validated decode
    /// (itself charged while retained): no file bytes, JSON tree or decoded
    /// copy is created, only the candidate, encoded output and transients.
    fn retained_bytes(self, aggregate: usize, input: usize) -> Option<usize> {
        let base = match self {
            Self::Replay | Self::Cursor | Self::ReceiptLookup => 2 * 1024 * 1024,
            Self::SnapshotSelect => aggregate.checked_add(2 * 1024 * 1024)?,
            Self::RegisterConsumer | Self::Acknowledge | Self::Maintenance => {
                aggregate.checked_mul(2)?.checked_add(128 * 1024)?
            }
            Self::SnapshotCommit | Self::SnapshotRenew | Self::SnapshotRelease => aggregate
                .checked_mul(2)?
                .checked_add(qos_relay::delivery_v3::commit::MAX_LEDGER_BYTES)?,
            Self::Onboarding => aggregate.checked_mul(3)?.checked_add(2 * 1024 * 1024)?,
            Self::State => aggregate
                .checked_mul(2)?
                .checked_add(input.checked_mul(2)?)?
                .checked_add(512 * 1024)?,
        };
        base.checked_add(256 * 1024)
    }
}
/// Largest staged resumable item accepted into one Store turn.
pub(crate) const MAX_STAGED_ITEM_BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloudDeliveryError {
    Capacity,
    Unavailable,
    Denied,
    Reset(ResetReason),
    InvalidInput,
    OwnerUncertain(crate::StoreCommitFailure),
}
impl std::fmt::Display for CloudDeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cloud delivery: {self:?}")
    }
}
impl std::error::Error for CloudDeliveryError {}
type Result<T> = std::result::Result<T, CloudDeliveryError>;
pub(super) struct DeliveryWork {
    pool: HostBytes,
    operation: DeliveryOperation,
    pub(super) input_len: usize,
    // Released only after the turn (and its Store lock/decoded state) drops.
    _credit: Option<ByteCredit>,
}
/// An exact current pin observation, never authority or a detached ref vector.
pub(crate) struct CloudPinExpectation {
    source: CloudStoreSource,
    store_generation: [u8; 16],
    id: [u8; 16],
    consumer: ConsumerId,
    policy: [u8; 32],
    watermark: CommitRef,
    expires_at_ns: u64,
    references: [u8; 32],
    count: usize,
}
impl CloudPinExpectation {
    pub(crate) fn expires_at_ns(&self) -> u64 {
        self.expires_at_ns
    }
}
pub(crate) struct CloudSnapshotRoots<'a> {
    pub(crate) baseline_generation: u64,
    pub(crate) baseline: &'a [PublicationRef],
    pub(crate) tail: &'a [PublicationRef],
}
impl CloudStoreSource {
    /// Production entry. The credit is sized from the persisted aggregate length
    /// before the Store is locked or decoded, and lives until the turn drops.
    pub(crate) fn with_delivery_turn<R>(
        &self,
        pool: &HostBytes,
        operation: DeliveryOperation,
        action: impl FnOnce(&mut CloudStoreTurn) -> Result<R>,
    ) -> Current<Result<R>> {
        if operation == DeliveryOperation::State {
            return Current::Current(Err(CloudDeliveryError::InvalidInput));
        }
        self.with_reserved_turn(pool, operation, 0, action)
    }
    /// State admission reserves its exact complete input length as well.
    pub(crate) fn with_state_turn<R>(
        &self,
        pool: &HostBytes,
        input_len: usize,
        action: impl FnOnce(&mut CloudStoreTurn) -> Result<R>,
    ) -> Current<Result<R>> {
        if input_len == 0 || input_len > MAX_STAGED_ITEM_BYTES {
            return Current::Current(Err(CloudDeliveryError::InvalidInput));
        }
        self.with_reserved_turn(pool, DeliveryOperation::State, input_len, action)
    }
    fn with_reserved_turn<R>(
        &self,
        pool: &HostBytes,
        operation: DeliveryOperation,
        input_len: usize,
        action: impl FnOnce(&mut CloudStoreTurn) -> Result<R>,
    ) -> Current<Result<R>> {
        if self.0.denied.load(std::sync::atomic::Ordering::Acquire) {
            return Current::Denied;
        }
        // The persisted aggregate bound is observed through the original bound
        // directory handle; a missing/unreadable aggregate is an owner failure.
        let aggregate = match persisted_aggregate_len(&self.0.directory) {
            Some(length) => length,
            None => return self.deny(),
        };
        self.bind_retained_pool(pool);
        // A retained decode needs no decode budget. If the hint turns out stale
        // under the lock, the decode difference is charged before any read,
        // or the turn reports Busy and the caller retries with a full budget.
        let full = operation.reserved_bytes(aggregate, input_len);
        let reused = self.retains_current();
        let reserved = if reused {
            operation.retained_bytes(aggregate, input_len)
        } else {
            full
        };
        let Some(credit) = reserved.and_then(|bytes| pool.try_acquire_bulk(bytes)) else {
            // Turn budgets scale with the persisted aggregate; name the sizes
            // so a capacity refusal is attributable (rate-limited).
            if let Some(suppressed) = super::service::ops_log::admit("turn_capacity") {
                tracing::warn!(
                    event = "cloud_turn_capacity",
                    ?operation,
                    reserved = reserved.unwrap_or(usize::MAX),
                    available = pool.available_bytes(),
                    aggregate,
                    reused,
                    suppressed,
                    "Cloud Store turn refused: host byte budget exhausted"
                );
            }
            return Current::Current(Err(CloudDeliveryError::Capacity));
        };
        let mut decode_credit = None;
        let mut admit_decode = || {
            if !reused {
                return true;
            }
            decode_credit = full
                .zip(reserved)
                .and_then(|(full, reserved)| full.checked_sub(reserved))
                .and_then(|bytes| pool.try_acquire_bulk(bytes));
            decode_credit.is_some()
        };
        self.with_current_admitting(&mut admit_decode, |turn| {
            turn.delivery_work = Some(DeliveryWork {
                pool: pool.clone(),
                operation,
                input_len,
                _credit: Some(credit),
            });
            action(turn)
        })
    }
    /// Actual Store and barrier fixture only, without the pool reservation.
    /// Component tests use it to exercise refusals independently of capacity.
    #[cfg(test)]
    pub(crate) fn with_delivery_turn_unqualified_test<R>(
        &self,
        pool: &HostBytes,
        operation: DeliveryOperation,
        action: impl FnOnce(&mut CloudStoreTurn) -> Result<R>,
    ) -> Current<Result<R>> {
        self.with_current_mut(|turn| {
            turn.delivery_work = Some(DeliveryWork {
                pool: pool.clone(),
                operation,
                input_len: MAX_STAGED_ITEM_BYTES,
                _credit: None,
            });
            action(turn)
        })
    }
}
/// Persisted aggregate bytes (checkpoint plus journal), unlocked: a sizing hint.
#[cfg(unix)]
fn persisted_aggregate_len(directory: &std::fs::File) -> Option<usize> {
    crate::storage::persisted_len_hint(directory).and_then(|length| usize::try_from(length).ok())
}
#[cfg(not(unix))]
fn persisted_aggregate_len(_: &std::fs::File) -> Option<usize> {
    None
}
/// Cheap commit observation: a checkpoint publication replaces `state.json`
/// and a journal append grows (or retirement replaces) `state.journal`.
/// This is only a wake-up hint; every disclosure still takes a fresh turn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct AggregateProbe {
    checkpoint: FileProbe,
    journal: Option<FileProbe>,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FileProbe {
    inode: u64,
    size: i64,
    modified_ns: i128,
}
#[cfg(unix)]
impl FileProbe {
    fn at(directory: &std::fs::File, name: &std::ffi::CStr) -> Option<Self> {
        let stat = crate::storage::stat_at(directory, name)?;
        Some(Self {
            inode: stat.st_ino as u64,
            size: stat.st_size as i64,
            modified_ns: i128::from(stat.st_mtime) * 1_000_000_000 + i128::from(stat.st_mtime_nsec),
        })
    }
}
impl CloudStoreSource {
    #[cfg(unix)]
    pub(crate) fn aggregate_probe(&self) -> Option<AggregateProbe> {
        Some(AggregateProbe {
            checkpoint: FileProbe::at(&self.0.directory, c"state.json")?,
            journal: FileProbe::at(&self.0.directory, c"state.journal"),
        })
    }
    #[cfg(not(unix))]
    pub(crate) fn aggregate_probe(&self) -> Option<AggregateProbe> {
        None
    }
    pub(crate) fn is_denied(&self) -> bool {
        self.0.denied.load(std::sync::atomic::Ordering::Acquire)
    }
}
impl CloudStoreTurn {
    pub(crate) fn require_delivery_operation(&self, operation: DeliveryOperation) -> Result<()> {
        match &self.delivery_work {
            Some(work) if work.operation == operation => Ok(()),
            _ => Err(CloudDeliveryError::Unavailable),
        }
    }
    pub(crate) fn host_bytes(&self) -> Result<&HostBytes> {
        self.delivery_work
            .as_ref()
            .map(|w| &w.pool)
            .ok_or(CloudDeliveryError::Unavailable)
    }
    pub(crate) fn native_work(&self) -> Result<&NativeWorkTurn> {
        Err(CloudDeliveryError::Unavailable)
    }
    pub(crate) fn delivery_ledger(&self) -> &DeliveryLedgerV1 {
        self.store
            .state
            .delivery
            .as_ref()
            .expect("selected turn")
            .ledger()
    }
    pub(crate) fn publication_pool(&self) -> &PublicationPool {
        self.store
            .state
            .delivery
            .as_ref()
            .expect("selected turn")
            .pool()
    }
    pub(crate) fn snapshot_roots(&self) -> Result<CloudSnapshotRoots<'_>> {
        self.host_bytes()?;
        let delivery = self
            .store
            .state
            .delivery
            .as_ref()
            .ok_or(CloudDeliveryError::Unavailable)?;
        let (baseline_generation, baseline, tail) = delivery.roots();
        Ok(CloudSnapshotRoots {
            baseline_generation,
            baseline,
            tail,
        })
    }
    pub(crate) fn delivery_policy<'a>(
        &'a self,
        session: &'a VerifiedSession,
    ) -> Result<DeliveryPolicyBasis<'a>> {
        self.host_bytes()?;
        let now = self
            .fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        let mut revocations = qos_user_identity::RevocationList::new();
        for entry in self.peer().revocations() {
            revocations
                .insert(entry)
                .map_err(|_| CloudDeliveryError::Denied)?;
        }
        session
            .revalidate(&auth::VerificationContext {
                binding: session.binding(),
                capabilities: auth::CapabilityPolicy {
                    supported: session.capabilities(),
                    allowed: session.capabilities(),
                },
                credentials: auth::Credentials {
                    device: session.device_certificate(),
                    host: self.peer().authority(),
                    device_revocations: &revocations,
                    host_revocations: &revocations,
                },
                authority: self,
                now_ns: now,
            })
            .map_err(|_| CloudDeliveryError::Denied)?;
        DeliveryPolicyBasis::inspect_unqualified(
            self.peer(),
            self.workspace(),
            self.peer().authority(),
            session,
            crate::now_time(now),
        )
        .map_err(|error| match error {
            PlanningFailure::CurrentDenied | PlanningFailure::UnsupportedPolicy => {
                CloudDeliveryError::Denied
            }
            PlanningFailure::Reset(reason) => CloudDeliveryError::Reset(reason),
            PlanningFailure::Capacity(_) => CloudDeliveryError::Capacity,
            PlanningFailure::Unavailable => CloudDeliveryError::Unavailable,
        })
    }
    pub(crate) fn commit_consumer(&mut self, consumer: ConsumerState) -> Result<()> {
        self.require_delivery_operation(DeliveryOperation::RegisterConsumer)?;
        self.commit_metadata(MetadataMutation::Consumer(consumer))
    }
    pub(crate) fn commit_ack(
        &mut self,
        device: [u8; 32],
        consumer: ConsumerId,
        cut: CommitRef,
    ) -> Result<()> {
        self.require_delivery_operation(DeliveryOperation::Acknowledge)?;
        self.commit_metadata(MetadataMutation::Ack {
            device,
            consumer,
            cut,
        })
    }
    pub(crate) fn sign_cursor(&self, issue: CursorIssue) -> Result<CursorToken> {
        self.host_bytes()?;
        self.fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        self.validate_cursor_issue(&issue)?;
        CursorToken::issue(issue, &self.store.key()).map_err(|_| CloudDeliveryError::InvalidInput)
    }
    pub(crate) fn authenticate_cursor(&self, cursor: &CursorToken) -> Result<SignedCursorClaims> {
        self.host_bytes()?;
        self.fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        cursor
            .authenticate(&self.store.key().verifying_key())
            .map_err(|_| CloudDeliveryError::InvalidInput)
    }
    pub(crate) fn snapshot_pin_expectation(
        &self,
        id: [u8; 16],
        scope: &CursorIssue,
        references: &PublicationCustody,
    ) -> Result<CloudPinExpectation> {
        self.host_bytes()?;
        self.fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        self.validate_cursor_issue(scope)?;
        let ledger = self.delivery_ledger();
        let pin = ledger
            .pins
            .get(&id)
            .ok_or(CloudDeliveryError::Reset(ResetReason::HistoryUnavailable))?;
        if !references.same_pool(self.host_bytes()?)
            || pin.consumer != scope.consumer
            || pin.authorization_view != scope.authorization_view
            || pin.watermark != scope.committed
            || pin.publications.len() != references.len()
            || pin
                .publications
                .iter()
                .enumerate()
                .any(|(index, item)| references.reference(index) != Some(item))
        {
            return Err(CloudDeliveryError::Denied);
        }
        Ok(CloudPinExpectation {
            source: self.source.clone(),
            store_generation: ledger.store_generation,
            id,
            consumer: pin.consumer,
            policy: pin.authorization_view,
            watermark: pin.watermark.clone(),
            expires_at_ns: pin.expires_at_ns,
            references: pin_digest(&pin.publications),
            count: pin.publications.len(),
        })
    }
    pub(crate) fn commit_snapshot_pin(
        &mut self,
        id: [u8; 16],
        scope: &CursorIssue,
        references: &PublicationCustody,
        expires_at_ns: u64,
    ) -> Result<()> {
        self.require_delivery_operation(DeliveryOperation::SnapshotCommit)?;
        let now = self
            .fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        self.validate_cursor_issue(scope)?;
        if id == [0; 16]
            || expires_at_ns <= now
            || expires_at_ns - now > 300_000_000_000
            || !references.same_pool(self.host_bytes()?)
            || references.is_empty()
        {
            return Err(CloudDeliveryError::InvalidInput);
        }
        let entry = self
            .delivery_ledger()
            .replay
            .get(&scope.committed.sequence)
            .ok_or(CloudDeliveryError::Reset(ResetReason::HistoryUnavailable))?;
        let exact_replay = entry.publications.len() == references.len()
            && entry
                .publications
                .iter()
                .enumerate()
                .all(|(index, item)| references.reference(index) == Some(item));
        let roots = self.snapshot_roots()?;
        let exact_baseline = if scope.committed.sequence < roots.baseline_generation {
            false
        } else {
            let count = self
                .delivery_ledger()
                .replay
                .range((
                    std::ops::Bound::Excluded(roots.baseline_generation),
                    std::ops::Bound::Included(scope.committed.sequence),
                ))
                .try_fold(roots.baseline.len(), |count, (_, item)| {
                    count.checked_add(item.publications.len())
                })
                .ok_or(CloudDeliveryError::Capacity)?;
            count == references.len()
                && roots.baseline.iter().chain(roots.tail).take(count).count() == count
                && roots
                    .baseline
                    .iter()
                    .chain(roots.tail)
                    .take(count)
                    .enumerate()
                    .all(|(index, item)| references.reference(index) == Some(item))
        };
        if !exact_replay && !exact_baseline {
            return Err(CloudDeliveryError::Reset(ResetReason::HistoryUnavailable));
        }
        let publications = (0..references.len())
            .map(|index| {
                references
                    .reference(index)
                    .cloned()
                    .ok_or(CloudDeliveryError::InvalidInput)
            })
            .collect::<Result<Vec<_>>>()?;
        self.commit_metadata(MetadataMutation::Pin {
            id,
            pin: SnapshotPin {
                consumer: scope.consumer,
                authorization_view: scope.authorization_view,
                watermark: scope.committed.clone(),
                publications,
                expires_at_ns,
            },
        })
    }
    pub(crate) fn renew_snapshot_pin(
        &mut self,
        id: [u8; 16],
        expected: &CloudPinExpectation,
        expires_at_ns: u64,
    ) -> Result<()> {
        self.require_delivery_operation(DeliveryOperation::SnapshotRenew)?;
        let now = self
            .fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        let pin = self.expected_pin(id, expected)?;
        if pin.expires_at_ns <= now
            || expires_at_ns <= pin.expires_at_ns
            || expires_at_ns
                .checked_sub(now)
                .is_none_or(|duration| duration > 300_000_000_000)
        {
            return Err(CloudDeliveryError::InvalidInput);
        }
        self.commit_metadata(MetadataMutation::RenewPin { id, expires_at_ns })
    }
    pub(crate) fn release_snapshot_pin(
        &mut self,
        id: [u8; 16],
        expected: &CloudPinExpectation,
    ) -> Result<()> {
        self.require_delivery_operation(DeliveryOperation::SnapshotRelease)?;
        self.expected_pin(id, expected)?;
        self.commit_metadata(MetadataMutation::ReleasePin { id })
    }
    /// Retire expired pins after restart or client abandonment. A pin past its
    /// signed expiry can never be renewed or read, so only quota is released.
    /// Durable snapshot pins that no live transfer of this process serves
    /// (`live` names every live transfer's pin), released in one metadata
    /// commit before a new pin is taken: all of `device`'s own (a device
    /// starting a new transfer has abandoned them; only a Renew could resume
    /// one, and that answers SnapshotRequired), and, while the table is
    /// nearly full, the oldest of any device down to half of `MAX_PINS`.
    /// Without this, pins of failed or restarted transfers stay until expiry
    /// and retrying clients fill the table, after which every snapshot of
    /// every device is refused with Capacity (observed in the lab after a
    /// host restart and after a delivery rotation).
    pub(crate) fn release_orphan_snapshot_pins(
        &mut self,
        device: [u8; 32],
        live: &[[u8; 16]],
    ) -> Result<Vec<[u8; 16]>> {
        use qos_relay::delivery_v3::commit::MAX_PINS;
        // Its own Release turn, or the draft turn of the new transfer.
        if self
            .require_delivery_operation(DeliveryOperation::SnapshotCommit)
            .is_err()
        {
            self.require_delivery_operation(DeliveryOperation::SnapshotRelease)?;
        }
        let ledger = self.delivery_ledger();
        let own = ledger.consumers.get(&device).map(|consumer| consumer.id);
        let mut orphans = ledger
            .pins
            .iter()
            .filter(|(id, _)| !live.contains(id))
            .map(|(id, pin)| (Some(pin.consumer) == own, pin.expires_at_ns, *id))
            .collect::<Vec<_>>();
        // Own pins first, then the oldest (earliest expiry) of other devices.
        orphans.sort_by_key(|(mine, expires, _)| (!*mine, *expires));
        let full = ledger.pins.len() + 1 >= MAX_PINS;
        let mut remaining = ledger.pins.len();
        let mut released = Vec::new();
        for (mine, _, id) in orphans {
            if mine || (full && remaining > MAX_PINS / 2) {
                released.push(id);
                remaining -= 1;
            }
        }
        if !released.is_empty() {
            self.commit_metadata(MetadataMutation::ReleasePins {
                ids: released.clone(),
            })?;
        }
        Ok(released)
    }
    pub(crate) fn expire_snapshot_pins(&mut self) -> Result<Vec<[u8; 16]>> {
        self.require_delivery_operation(DeliveryOperation::Maintenance)?;
        let now = self
            .fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        let expired = self
            .delivery_ledger()
            .pins
            .iter()
            .filter(|(_, pin)| pin.expires_at_ns <= now)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        if !expired.is_empty() {
            self.commit_metadata(MetadataMutation::ExpirePins { now })?;
        }
        Ok(expired)
    }
    pub(crate) fn sign_snapshot_manifest(
        &self,
        issue: ManifestIssue,
        stream: &SnapshotStream,
    ) -> Result<SnapshotManifest> {
        match self.delivery_work.as_ref().map(|work| work.operation) {
            Some(DeliveryOperation::SnapshotCommit) => (),
            _ => return Err(CloudDeliveryError::Unavailable),
        }
        let now = self
            .fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        self.validate_cursor_issue(&issue.scope)?;
        let references = stream.custody();
        if !references.same_pool(self.host_bytes()?)
            || issue.item_count as usize != stream.item_count()
            || issue.stream_bytes != stream.stream_bytes()
            || issue.chunk_size as usize != SNAPSHOT_CHUNK_BYTES
            || issue.chunk_digests != stream.chunk_digests()
        {
            return Err(CloudDeliveryError::InvalidInput);
        }
        let ledger = self.delivery_ledger();
        let entry = ledger
            .replay
            .get(&issue.scope.committed.sequence)
            .ok_or(CloudDeliveryError::Reset(ResetReason::HistoryUnavailable))?;
        let retained = match issue.kind {
            SnapshotKind::Baseline => {
                let roots = self.snapshot_roots()?;
                if issue.scope.committed.sequence < roots.baseline_generation {
                    return Err(CloudDeliveryError::Reset(ResetReason::HistoryUnavailable));
                }
                // A concurrent suffix does not change the original baseline cut.
                // Count the exact published groups through that selected watermark.
                let count = ledger
                    .replay
                    .range((
                        std::ops::Bound::Excluded(roots.baseline_generation),
                        std::ops::Bound::Included(issue.scope.committed.sequence),
                    ))
                    .try_fold(roots.baseline.len(), |count, (_, item)| {
                        count.checked_add(item.publications.len())
                    })
                    .ok_or(CloudDeliveryError::Capacity)?;
                issue.predecessor.is_none()
                    && issue.baseline_generation == roots.baseline_generation
                    && issue.scope.committed.sequence >= roots.baseline_generation
                    && references.len() == count
                    && roots.baseline.iter().chain(roots.tail).take(count).count() == count
                    && roots
                        .baseline
                        .iter()
                        .chain(roots.tail)
                        .take(count)
                        .enumerate()
                        .all(|(index, reference)| references.reference(index) == Some(reference))
            }
            SnapshotKind::Replay => {
                issue.baseline_generation == entry.owner_generation
                    && issue.predecessor == entry.previous
                    && references.len() == entry.publications.len()
                    && entry
                        .publications
                        .iter()
                        .enumerate()
                        .all(|(index, reference)| references.reference(index) == Some(reference))
            }
        };
        if !retained
            || (0..references.len()).any(|index| {
                references.reference(index).is_none_or(|reference| {
                    self.publication_pool().resolve(&reference.payload).is_err()
                })
            })
        {
            return Err(CloudDeliveryError::Reset(ResetReason::HistoryUnavailable));
        }
        SnapshotManifest::issue(issue, &self.store.key(), now)
            .map_err(|_| CloudDeliveryError::InvalidInput)
    }
    pub(crate) fn renew_snapshot_manifest(
        &self,
        pair: &SnapshotManifestPair,
        expected: &CloudPinExpectation,
        stream: &SnapshotStream,
        expires_at_ns: u64,
    ) -> Result<SnapshotManifest> {
        self.require_delivery_operation(DeliveryOperation::SnapshotRenew)?;
        let now = self
            .fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        let pin = self.expected_pin(expected.id, expected)?;
        if pin.expires_at_ns <= now
            || expires_at_ns <= pin.expires_at_ns
            || expires_at_ns
                .checked_sub(now)
                .is_none_or(|duration| duration > 300_000_000_000)
        {
            return Err(CloudDeliveryError::InvalidInput);
        }
        let signed = self.authenticate_snapshot_pair_retained(pair)?;
        let old = signed.current().issue();
        self.validate_cursor_issue(&old.scope)?;
        let references = stream.custody();
        if old.pin != expected.id
            || old.scope.consumer != pin.consumer
            || old.scope.authorization_view != pin.authorization_view
            || old.scope.committed != pin.watermark
            || old.expires_at_ns != pin.expires_at_ns
            || !references.same_pool(self.host_bytes()?)
            || references.len() != pin.publications.len()
            || pin
                .publications
                .iter()
                .enumerate()
                .any(|(index, item)| references.reference(index) != Some(item))
            || old.item_count as usize != stream.item_count()
            || old.stream_bytes != stream.stream_bytes()
            || old.chunk_size as usize != SNAPSHOT_CHUNK_BYTES
            || old.chunk_digests != stream.chunk_digests()
        {
            return Err(CloudDeliveryError::Denied);
        }
        // The caller's retained pair/manifest workspace charge remains live.
        // Derive every immutable field from the actual original host signature.
        let mut issue = old.clone();
        issue.expires_at_ns = expires_at_ns;
        SnapshotManifest::issue(issue, &self.store.key(), now)
            .map_err(|_| CloudDeliveryError::InvalidInput)
    }
    fn expected_pin<'a>(
        &'a self,
        id: [u8; 16],
        expected: &CloudPinExpectation,
    ) -> Result<&'a SnapshotPin> {
        self.host_bytes()?;
        self.fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        let ledger = self.delivery_ledger();
        if !expected.source.owns_turn(self)
            || expected.store_generation != ledger.store_generation
            || expected.id != id
        {
            return Err(CloudDeliveryError::Denied);
        }
        let pin = ledger
            .pins
            .get(&id)
            .ok_or(CloudDeliveryError::Reset(ResetReason::HistoryUnavailable))?;
        if pin.consumer != expected.consumer
            || pin.authorization_view != expected.policy
            || pin.watermark != expected.watermark
            || pin.expires_at_ns != expected.expires_at_ns
            || pin.publications.len() != expected.count
            || pin_digest(&pin.publications) != expected.references
        {
            return Err(CloudDeliveryError::Denied);
        }
        Ok(pin)
    }
    /// Authenticate retained signed claims without choosing a live pin. The
    /// caller must select the actual ledger pin and validate its current expiry.
    pub(crate) fn authenticate_snapshot_pair_retained(
        &self,
        pair: &SnapshotManifestPair,
    ) -> Result<RetainedAuthenticatedSnapshotPair> {
        self.host_bytes()?;
        self.fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        pair.authenticate_retained(&self.store.key().verifying_key())
            .map_err(|_| CloudDeliveryError::InvalidInput)
    }
    pub(crate) fn authenticate_snapshot_pair(
        &self,
        pair: &SnapshotManifestPair,
    ) -> Result<AuthenticatedSnapshotPair> {
        self.host_bytes()?;
        let now = self
            .fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        pair.authenticate(&self.store.key().verifying_key(), now)
            .map_err(|_| CloudDeliveryError::InvalidInput)
    }
    fn validate_cursor_issue(&self, issue: &CursorIssue) -> Result<()> {
        let host = self.peer().authority();
        let ledger = self.delivery_ledger();
        let consumer = ledger
            .consumers
            .get(&issue.consumer_device)
            .ok_or(CloudDeliveryError::Denied)?;
        let entry = ledger
            .replay
            .get(&issue.committed.sequence)
            .ok_or(CloudDeliveryError::Reset(ResetReason::CursorUnknown))?;
        if issue.room != self.peer().room()
            || issue.workspace != self.workspace()
            || issue.workspace_root != host.user_public_key
            || issue.host_device != host.device_public_key
            || issue.host_root != host.user_public_key
            || issue.store_generation != ledger.store_generation
            || consumer.id != issue.consumer
            || consumer.root != issue.consumer_root
            || consumer.authorization_view != issue.authorization_view
            || entry.digest != issue.committed.digest
            || entry.owner_generation != issue.committed.sequence
        {
            return Err(CloudDeliveryError::Denied);
        }
        Ok(())
    }
}

// Metadata-only ledger mutations share the original Store commit barrier. The
// turn is refreshed from the committed state before any caller observes it.
enum MetadataMutation {
    Consumer(ConsumerState),
    RenewPin {
        id: [u8; 16],
        expires_at_ns: u64,
    },
    ReleasePin {
        id: [u8; 16],
    },
    /// Release several pins at once; ids already gone are skipped.
    ReleasePins {
        ids: Vec<[u8; 16]>,
    },
    Pin {
        id: [u8; 16],
        pin: SnapshotPin,
    },
    Ack {
        device: [u8; 32],
        consumer: ConsumerId,
        cut: CommitRef,
    },
    ExpirePins {
        now: u64,
    },
}
impl CloudStoreTurn {
    fn commit_metadata(&mut self, change: MetadataMutation) -> Result<()> {
        self.fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        let mut next = self.store.state.clone();
        let ledger = next
            .delivery
            .as_mut()
            .ok_or(CloudDeliveryError::Unavailable)?
            .ledger_mut();
        match change {
            MetadataMutation::ExpirePins { now } => {
                let before = ledger.pins.len();
                ledger.pins.retain(|_, pin| pin.expires_at_ns > now);
                if ledger.pins.len() == before {
                    return Ok(());
                }
            }
            MetadataMutation::RenewPin { id, expires_at_ns } => {
                let pin = ledger
                    .pins
                    .get_mut(&id)
                    .ok_or(CloudDeliveryError::Reset(ResetReason::HistoryUnavailable))?;
                pin.expires_at_ns = expires_at_ns;
            }
            MetadataMutation::ReleasePin { id } => {
                ledger
                    .pins
                    .remove(&id)
                    .ok_or(CloudDeliveryError::Reset(ResetReason::HistoryUnavailable))?;
            }
            MetadataMutation::ReleasePins { ids } => {
                let before = ledger.pins.len();
                for id in &ids {
                    ledger.pins.remove(id);
                }
                if ledger.pins.len() == before {
                    return Ok(());
                }
            }
            MetadataMutation::Pin { id, pin } => {
                if let Some(previous) = ledger.pins.get(&id) {
                    if previous == &pin {
                        return Ok(());
                    }
                    return Err(CloudDeliveryError::Denied);
                }
                if ledger.pins.len() >= qos_relay::delivery_v3::commit::MAX_PINS {
                    return Err(CloudDeliveryError::Capacity);
                }
                ledger.pins.insert(id, pin);
            }
            MetadataMutation::Consumer(consumer) => {
                if consumer.id == [0; 16] || consumer.acknowledged.is_some() {
                    return Err(CloudDeliveryError::InvalidInput);
                }
                if let Some(old) = ledger.consumers.get(&consumer.device) {
                    if old.id != consumer.id || old.root != consumer.root {
                        return Err(CloudDeliveryError::Denied);
                    }
                    if old == &consumer {
                        return Ok(());
                    }
                } else if ledger.consumers.values().any(|old| old.id == consumer.id) {
                    return Err(CloudDeliveryError::InvalidInput);
                }
                ledger.consumers.insert(consumer.device, consumer);
            }
            MetadataMutation::Ack {
                device,
                consumer,
                cut,
            } => {
                let entry = ledger
                    .replay
                    .get(&cut.sequence)
                    .ok_or(CloudDeliveryError::Reset(ResetReason::CursorUnknown))?;
                if entry.digest != cut.digest || entry.owner_generation != cut.sequence {
                    return Err(CloudDeliveryError::Denied);
                }
                let current = ledger
                    .consumers
                    .get_mut(&device)
                    .ok_or(CloudDeliveryError::Denied)?;
                if current.id != consumer {
                    return Err(CloudDeliveryError::Denied);
                }
                if let Some(old) = &current.acknowledged {
                    if old.sequence > cut.sequence {
                        return Err(CloudDeliveryError::InvalidInput);
                    }
                    if old == &cut {
                        return Ok(());
                    }
                }
                current.acknowledged = Some(cut);
            }
        }
        if let Err(error) = self.store.commit(next) {
            if let Some(failure) = error.downcast_ref::<crate::StoreCommitFailure>() {
                let failure = *failure;
                self.source.deny::<()>();
                return Err(CloudDeliveryError::OwnerUncertain(failure));
            }
            return Err(CloudDeliveryError::Unavailable);
        }
        self.now_ns = self
            .fresh_now_ns()
            .map_err(|_| CloudDeliveryError::Denied)?;
        self.peer = crate::delivery::committed_peer(&self.store.state, self.now_ns)
            .map_err(|_| CloudDeliveryError::Denied)?;
        Ok(())
    }
}

fn pin_digest(references: &[PublicationRef]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"qos-cloud-pin-expectation-v1\0");
    hash.update(&(references.len() as u64).to_le_bytes());
    for reference in references {
        hash.update(&reference.payload);
        hash.update(&[match reference.kind {
            qos_peer_state::transport::Retention::Checkpoint => 0,
            qos_peer_state::transport::Retention::Delta => 1,
        }]);
    }
    *hash.finalize().as_bytes()
}

#[cfg(test)]
mod sizing_tests {
    use super::*;
    const MIB: usize = 1024 * 1024;
    /// The shared ingress pool the Cloud host receives from its transport.
    const POOL: usize = 256 * MIB;
    /// Headroom kept for the control lane, connections and concurrent reads.
    const HEADROOM: usize = 16 * MIB;

    /// Pool demand while one maximal staged item is admitted: its read-back
    /// buffer, the retained decode charge and the State turn reservation.
    fn staged_item_demand(aggregate: usize, retained: bool) -> usize {
        let item = MAX_STAGED_ITEM_BYTES;
        let turn = if retained {
            DeliveryOperation::State.retained_bytes(aggregate, item)
        } else {
            DeliveryOperation::State.reserved_bytes(aggregate, item)
        }
        .unwrap();
        let retained_charge = if retained { 2 * aggregate + MIB } else { 0 };
        item + turn + retained_charge
    }

    #[test]
    fn retained_turns_do_not_pay_for_a_decode_they_never_perform() {
        let aggregate = 32 * MIB;
        for operation in [
            DeliveryOperation::Replay,
            DeliveryOperation::Cursor,
            DeliveryOperation::ReceiptLookup,
            DeliveryOperation::SnapshotSelect,
            DeliveryOperation::Acknowledge,
            DeliveryOperation::RegisterConsumer,
            DeliveryOperation::Maintenance,
            DeliveryOperation::SnapshotCommit,
            DeliveryOperation::Onboarding,
        ] {
            let full = operation.reserved_bytes(aggregate, 0).unwrap();
            let retained = operation.retained_bytes(aggregate, 0).unwrap();
            assert!(retained < full, "{operation:?}");
        }
        // Reads over a 64 MiB aggregate stay admissible with a retained decode
        // (the full-decode budget alone would exceed most of the pool).
        let read = DeliveryOperation::Replay
            .retained_bytes(64 * MIB, 0)
            .unwrap();
        assert!(read + 2 * 64 * MIB + MIB <= POOL - HEADROOM);
        assert!(
            DeliveryOperation::Replay
                .reserved_bytes(64 * MIB, 0)
                .unwrap()
                > POOL / 2
        );
    }

    /// A maximal 64 MiB resumable item is admissible only while the aggregate
    /// stays small: the 256 MiB shared pool must hold the item, its decoded and
    /// candidate copies and the aggregate candidate/encoding. This records the
    /// actual bound instead of implying 64 MiB items always fit.
    #[test]
    fn maximal_staged_item_fits_the_shared_pool_only_below_the_recorded_aggregate() {
        let budget = POOL - HEADROOM;
        assert!(staged_item_demand(8 * MIB, true) <= budget);
        assert!(staged_item_demand(8 * MIB, false) <= budget);
        let largest = (0..=64)
            .map(|mib| mib * MIB)
            .take_while(|aggregate| staged_item_demand(*aggregate, true) <= budget)
            .last()
            .unwrap();
        assert!((8 * MIB..16 * MIB).contains(&largest), "{}", largest / MIB);
        // Beyond it the turn refuses with a retryable capacity answer.
        assert!(staged_item_demand(32 * MIB, true) > budget);
    }
}
