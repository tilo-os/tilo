//! Serialized-byte custody on the existing host pool. Never a decoded-state qualifier.
use super::delivery::error::{
    DataResult, OwnerCapacityKind as Capacity, OwnerDataFailure, OwnerDataFailureKind as Failure,
};
use super::*;
use crate::host_bytes::{ByteCredit, HostBytes};
use std::collections::VecDeque;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

pub(super) const POLICY_BYTES: usize = 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnerDataOperation {
    RegisterConsumer,
    ReceiptLookup,
    SnapshotPrepare,
    SnapshotCreate,
    SnapshotRenew,
    SnapshotRelease,
    PrivateContext { input_len: usize },
    State { input_len: usize },
    StateStaged { input_len: usize },
    Ack,
    Replay,
}
/// These caps constrain owner serialized buffers only. Native decode/save/clone
/// needs its separate qps qualifier before a route can become data-ready.
#[derive(Clone, Copy)]
struct SerializedCaps {
    _aggregate: usize,
    _transient: usize,
    _retained: usize,
}
pub(crate) struct OwnerResourceTurn {
    retained: Arc<RetainedSerializedBytes>,
    generation: u64,
    operation: OwnerDataOperation,
    _caps: SerializedCaps,
    _scratch: ByteCredit,
    active: Arc<ActiveTurn>,
    used: bool,
    _large_decode: Option<crate::store::LargeDecodeLease>,
}
pub(super) struct OwnerResources {
    budget: HostBytes,
    retained: Arc<RetainedSerializedBytes>,
    active: Arc<Mutex<Option<Arc<ActiveTurn>>>>,
    /// Set only by the production host, whose relay, collaboration, worker
    /// and descriptor paths are not yet owner-scoped. Otherwise an attached
    /// owner refuses every mutation outside a turn or scope.
    unscoped_legacy: bool,
}
struct ActiveTurn {
    cap: usize,
    retained_allowance: Mutex<ByteCredit>,
    staged: Mutex<StagedCredits>,
}
/// Retained credits split for one commit, installed only after its barrier.
#[derive(Default)]
struct StagedCredits {
    /// Exact retained capacity of the candidate aggregate (buffers + pool).
    generation: Option<ByteCredit>,
    /// Backing of the candidate's live publication artifact.
    live: Option<ByteCredit>,
}
struct ActiveReset(Arc<Mutex<Option<Arc<ActiveTurn>>>>);
impl Drop for ActiveReset {
    fn drop(&mut self) {
        if let Ok(mut active) = self.0.lock() {
            *active = None;
        }
    }
}
/// One held credit that pays for one concrete retained allocation and
/// returns both its permits and its accounted bytes when that holder drops.
pub(crate) struct AccountedCredit {
    credit: ByteCredit,
    accounted: Arc<AtomicUsize>,
}
impl AccountedCredit {
    fn new(credit: ByteCredit, accounted: &Arc<AtomicUsize>) -> Self {
        // Every held credit comes from the same bounded semaphore; the sum
        // cannot exceed its representable permit capacity.
        accounted.fetch_add(credit.bytes(), Ordering::Relaxed);
        Self {
            credit,
            accounted: accounted.clone(),
        }
    }
    fn bytes(&self) -> usize {
        self.credit.bytes()
    }
    /// Return `bytes` of this sole-owned credit to the pool.
    fn shrink(&mut self, bytes: usize) {
        if let Some(excess) = self.credit.split(bytes) {
            self.accounted.fetch_sub(excess.bytes(), Ordering::Relaxed);
        }
    }
    fn grow(&mut self, extra: ByteCredit) {
        self.accounted.fetch_add(extra.bytes(), Ordering::Relaxed);
        self.credit.merge(extra);
    }
}
impl Drop for AccountedCredit {
    fn drop(&mut self) {
        self.accounted
            .fetch_sub(self.credit.bytes(), Ordering::Relaxed);
    }
}
impl std::fmt::Debug for AccountedCredit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountedCredit")
            .field("bytes", &self.bytes())
            .finish()
    }
}
/// Owner-lifetime serialized custody. Each credit is tied to the allocation
/// it pays for and is released when that allocation is freed:
/// - `generation`: the committed aggregate's buffers and publication pool
///   payloads. Each durable commit replaces it with the next aggregate's
///   exact retained capacity; escaped pins keep their generation alive.
/// - `pending`: inputs retained by the native deferred queue, released
///   oldest-first as that queue drains.
/// - `live`: the last commit's live publication backing until the owner
///   installs it into the (possibly escaping) live artifact.
///
/// Views and pins also hold this Arc, so the current generation stays paid
/// until the owner and every escaped holder are gone.
pub(crate) struct RetainedSerializedBytes {
    generation: Mutex<Arc<AccountedCredit>>,
    pending: Mutex<VecDeque<AccountedCredit>>,
    live: Mutex<Option<AccountedCredit>>,
    bytes: Arc<AtomicUsize>,
}
impl RetainedSerializedBytes {
    /// The credit paying for the current generation's retained buffers and
    /// publication payloads; a pin holds it for the payloads it borrows.
    pub(crate) fn current_generation(&self) -> Arc<AccountedCredit> {
        self.generation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
    fn push_pending(&self, credit: ByteCredit) {
        let credit = AccountedCredit::new(credit, &self.bytes);
        self.pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push_back(credit);
    }
}
impl std::fmt::Debug for RetainedSerializedBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetainedSerializedBytes")
            .field("bytes", &self.bytes.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}
fn capacity() -> OwnerDataFailure {
    OwnerDataFailure::new(
        Failure::Capacity(Capacity::Bytes),
        anyhow::anyhow!("serialized byte allowance unavailable"),
    )
}
fn add(a: usize, b: usize) -> DataResult<usize> {
    a.checked_add(b).ok_or_else(capacity)
}
fn multiply(a: usize, b: usize) -> DataResult<usize> {
    a.checked_mul(b).ok_or_else(capacity)
}
fn disk(error: crate::store::StorePreflightFailure) -> OwnerDataFailure {
    let kind = match error {
        crate::store::StorePreflightFailure::Capacity => Failure::Capacity(Capacity::Disk),
        crate::store::StorePreflightFailure::Unavailable => Failure::Unavailable,
    };
    OwnerDataFailure::new(kind, error)
}
impl SessionOwner {
    /// Attach existing raw owner buffers before any candidate allocation. This
    /// intentionally confers no readiness for native structures/caches.
    pub(crate) fn attach_data_resources(&mut self, budget: &HostBytes) -> DataResult<()> {
        if let Some(resources) = &self.resources {
            return if resources.budget.same_pool(budget) {
                Ok(())
            } else {
                Err(OwnerDataFailure::new(
                    Failure::Unavailable,
                    anyhow::anyhow!("owner already attached to another host pool"),
                ))
            };
        }
        let retained =
            retained_capacity(&self.aggregate).map_err(|e| self.classify_data_error(e))?;
        let live = if let Some(live) = &self.live_publication_transition {
            if Arc::strong_count(live) != 1 || Arc::weak_count(live) != 0 {
                return Err(OwnerDataFailure::new(
                    Failure::Unavailable,
                    anyhow::anyhow!("unaccounted live publication already escaped"),
                ));
            }
            let mut bytes = add(
                live.room.capacity(),
                multiply(live.messages.len(), size_of::<(Vec<u8>, Retention)>())?,
            )?;
            for (payload, _) in live.messages.iter() {
                bytes = add(bytes, payload.capacity())?;
            }
            bytes
        } else {
            0
        };
        let mut credit = budget
            .try_acquire_bulk(add(retained, live)?)
            .ok_or_else(capacity)?;
        let live_credit = credit.split(live).ok_or_else(capacity)?;
        let accounted = Arc::new(AtomicUsize::new(0));
        let generation = Arc::new(AccountedCredit::new(credit, &accounted));
        self.resources = Some(OwnerResources {
            budget: budget.clone(),
            unscoped_legacy: false,
            active: Arc::new(Mutex::new(None)),
            retained: Arc::new(RetainedSerializedBytes {
                generation: Mutex::new(generation),
                pending: Mutex::new(VecDeque::new()),
                live: Mutex::new(None),
                bytes: accounted.clone(),
            }),
        });
        let credit = self.retained_serialized_credit();
        if let Some(live) = &mut self.live_publication_transition {
            let live = Arc::get_mut(live).expect("sole checked live publication owner");
            live._serialized_credit = credit;
            live._backing = Some(Arc::new(AccountedCredit::new(live_credit, &accounted)));
        }
        Ok(())
    }
    pub(crate) fn prepare_data_turn(
        &self,
        budget: &HostBytes,
        operation: OwnerDataOperation,
    ) -> DataResult<OwnerResourceTurn> {
        if matches!(operation, OwnerDataOperation::StateStaged { .. }) {
            return Err(OwnerDataFailure::new(
                Failure::CurrentDenied,
                anyhow::anyhow!("staged input requires the original shared decode inventory"),
            ));
        }
        self.prepare_data_turn_inner(budget, operation, None)
    }
    fn prepare_data_turn_inner(
        &self,
        budget: &HostBytes,
        operation: OwnerDataOperation,
        large_decode: Option<crate::store::LargeDecodeLease>,
    ) -> DataResult<OwnerResourceTurn> {
        self.require_delivery()
            .map_err(|e| self.classify_data_error(e))?;
        let resources = self
            .resources
            .as_ref()
            .filter(|r| r.budget.same_pool(budget))
            .ok_or_else(|| {
                OwnerDataFailure::new(
                    Failure::Unavailable,
                    anyhow::anyhow!("owner serialized resource custody is not attached"),
                )
            })?;
        let current = schema::signed_size(
            self.aggregate
                .encoded_payload_len()
                .map_err(|e| self.classify_data_error(e))?,
        )
        .map_err(|e| self.classify_data_error(e))?;
        let input = match operation {
            OwnerDataOperation::State { input_len }
                if input_len > 0 && input_len <= delivery::MAX_DELIVERY_ITEM_BYTES =>
            {
                input_len
            }
            OwnerDataOperation::StateStaged { input_len }
                if large_decode.is_some() && input_len > 0 && input_len <= 64 * 1024 * 1024 =>
            {
                input_len
            }
            OwnerDataOperation::PrivateContext { input_len } if input_len <= 256 * 1024 => {
                input_len
            }
            OwnerDataOperation::State { .. }
            | OwnerDataOperation::StateStaged { .. }
            | OwnerDataOperation::PrivateContext { .. } => {
                return Err(OwnerDataFailure::new(
                    Failure::InvalidNativeInput,
                    anyhow::anyhow!("state length outside complete input bound"),
                ));
            }
            _ => 0,
        };
        // Per-turn output admission policy, enforced again before owner output
        // serialization. This is not an empirical native-memory multiplier.
        let aggregate = match operation {
            OwnerDataOperation::State { .. } => {
                add(add(multiply(current, 2)?, multiply(input, 2)?)?, 512 * 1024)?
            }
            OwnerDataOperation::StateStaged { .. } => {
                // Explicit serialized output admission budget for the durable
                // native peer plus certified publication. Exact counted output
                // must still fit; this is not a native heap estimate/qualifier.
                add(add(multiply(current, 2)?, multiply(input, 4)?)?, 512 * 1024)?
            }
            OwnerDataOperation::SnapshotCreate
            | OwnerDataOperation::SnapshotRenew
            | OwnerDataOperation::SnapshotRelease => {
                add(current, tilo_relay::delivery_v3::commit::MAX_LEDGER_BYTES)?
            }
            OwnerDataOperation::RegisterConsumer | OwnerDataOperation::Ack => {
                add(current, 128 * 1024)?
            }
            OwnerDataOperation::Replay
            | OwnerDataOperation::ReceiptLookup
            | OwnerDataOperation::SnapshotPrepare
            | OwnerDataOperation::PrivateContext { .. } => current,
        }
        .min(crate::store::MAX_AGGREGATE_BYTES);
        if !matches!(
            operation,
            OwnerDataOperation::Replay
                | OwnerDataOperation::ReceiptLookup
                | OwnerDataOperation::SnapshotPrepare
                | OwnerDataOperation::PrivateContext { .. }
        ) {
            self.store.preflight(aggregate).map_err(disk)?;
        }
        let raw = common_raw_capacity(&self.aggregate).map_err(|e| self.classify_data_error(e))?;
        let mutating = !matches!(
            operation,
            OwnerDataOperation::Replay
                | OwnerDataOperation::ReceiptLookup
                | OwnerDataOperation::SnapshotPrepare
                | OwnerDataOperation::PrivateContext { .. }
        );
        // Sized from measured peaks, not a worst-case multiplier: the
        // long-run tests (delivery_resource_long_run_*) meter every allocation
        // of real State/ACK commits including baseline replacement, peaking
        // at <= 2.8x / 2.3x the current aggregate plus < 1 MiB. Native DTO and
        // structure allocations remain a separate qps reservation.
        //
        // Mutations: transient covers the candidate/encoded copies
        // (max(A, 2C) + C) plus scratch; retained funds the next aggregate's
        // exact retained capacity (<= A + its raw slack) and a live artifact.
        let (transient, retained) = if mutating {
            let copies = add(
                aggregate.max(add(multiply(current, 2)?, 512 * 1024)?),
                current,
            )?;
            (
                add(add(copies, raw)?, add(POLICY_BYTES, multiply(input, 2)?)?)?,
                add(
                    add(aggregate, raw)?,
                    add(aggregate.min(MAX_PUBLICATION_TAIL_BYTES), input)?,
                )?,
            )
        } else if operation == OwnerDataOperation::Replay {
            // Replay borrows pool payloads: one page, one complete group
            // (each <= PAGE_BYTES), its decoded events and policy scratch.
            // Measured poll peaks are tens of KiB; independent of aggregate.
            let page = tilo_relay::delivery_v3::capabilities::PAGE_BYTES;
            (add(multiply(page, 4)?, multiply(POLICY_BYTES, 3)?)?, 0)
        } else {
            (
                add(
                    add(multiply(aggregate, 6)?, multiply(raw, 2)?)?,
                    add(multiply(POLICY_BYTES, 3)?, multiply(input, 2)?)?,
                )?,
                0,
            )
        };
        let total = add(transient, retained)?;
        let mut credit = budget.try_acquire_bulk(total).ok_or_else(capacity)?;
        let retained_allowance = credit.split(retained).ok_or_else(capacity)?;
        Ok(OwnerResourceTurn {
            retained: resources.retained.clone(),
            generation: self.aggregate.generation,
            operation,
            _caps: SerializedCaps {
                _aggregate: aggregate,
                _transient: transient,
                _retained: retained,
            },
            _scratch: credit,
            active: Arc::new(ActiveTurn {
                cap: aggregate,
                retained_allowance: Mutex::new(retained_allowance),
                staged: Mutex::new(StagedCredits::default()),
            }),
            used: false,
            _large_decode: large_decode,
        })
    }
    pub(crate) fn classify_data_error(&self, error: anyhow::Error) -> OwnerDataFailure {
        if self.store.is_poisoned() {
            return OwnerDataFailure::new(Failure::DurabilityUncertain, error);
        }
        match error.downcast::<OwnerDataFailure>() {
            Ok(error) => error,
            Err(error) => {
                let kind = match error.downcast_ref::<crate::store::StorePreflightFailure>() {
                    Some(crate::store::StorePreflightFailure::Capacity) => {
                        Failure::Capacity(Capacity::Disk)
                    }
                    _ => Failure::Unavailable,
                };
                OwnerDataFailure::new(kind, error)
            }
        }
    }
}
fn common_raw_capacity(aggregate: &Aggregate) -> Result<usize> {
    let mut bytes = 0usize;
    let mut add = |value: usize| -> Result<()> {
        bytes = bytes
            .checked_add(value)
            .context("owner backing capacity overflow")?;
        Ok(())
    };
    add(aggregate.peer.capacity())?;
    add(aggregate.directory.capacity())?;
    add(aggregate.identity_root.capacity())?;
    add(aggregate.host_identity_root.capacity())?;
    add(aggregate.room.capacity())?;
    add(aggregate.authority.device_label.capacity())?;
    add(aggregate
        .pending
        .capacity()
        .checked_mul(size_of::<PendingOperation>())
        .context("pending slots overflow")?)?;
    for pending in &aggregate.pending {
        add(pending.encoded_request.capacity())?;
        add(pending.invocation_id.capacity())?;
    }
    add(aggregate
        .issuances
        .capacity()
        .checked_mul(size_of::<crate::enrollment::Issuance>())
        .context("issuance slots overflow")?)?;
    for issuance in &aggregate.issuances {
        add(issuance.certificate.device_label.capacity())?;
    }
    add(aggregate
        .collaboration
        .retained_serialized_capacity()
        .map_err(anyhow::Error::msg)?)?;
    let vector = |capacity: usize, element: usize| {
        capacity
            .checked_mul(element)
            .context("publication metadata slots overflow")
    };
    match &aggregate.publication {
        PublicationStorage::V5 {
            roots,
            delivery,
            transport_hint,
            ..
        } => {
            use tilo_relay::delivery_v3::commit::{Digest, PublicationRef};
            add(vector(
                roots.baseline.capacity(),
                size_of::<PublicationRef>(),
            )?)?;
            add(vector(roots.tail.capacity(), size_of::<PublicationRef>())?)?;
            for entry in delivery.replay.values() {
                add(vector(
                    entry.publications.capacity(),
                    size_of::<PublicationRef>(),
                )?)?;
                add(vector(
                    entry.admitted_operations.capacity(),
                    size_of::<Digest>(),
                )?)?;
            }
            for receipt in delivery.admissions.values() {
                add(vector(receipt.operations.capacity(), size_of::<Digest>())?)?;
            }
            for pin in delivery.pins.values() {
                add(vector(
                    pin.publications.capacity(),
                    size_of::<PublicationRef>(),
                )?)?;
            }
            if let Some(hint) = transport_hint {
                add(hint.kind.capacity())?;
                add(hint.opaque_json.capacity())?;
                add(hint.host.device_label.capacity())?;
            }
        }
        PublicationStorage::Legacy { outgoing, state } => {
            add(vector(outgoing.capacity(), size_of::<Vec<u8>>())?)?;
            add(vector(state.kinds.capacity(), size_of::<Retention>())?)?;
        }
    }
    Ok(bytes)
}
fn retained_capacity(aggregate: &Aggregate) -> Result<usize> {
    let mut bytes = common_raw_capacity(aggregate)?;
    match &aggregate.publication {
        PublicationStorage::V5 { pool, .. } => {
            bytes = bytes
                .checked_add(pool.retained_payload_capacity()?)
                .context("retained pool size overflow")?
        }
        PublicationStorage::Legacy { outgoing, .. } => {
            for value in outgoing {
                bytes = bytes
                    .checked_add(value.capacity())
                    .context("retained publication size overflow")?;
            }
        }
    }
    Ok(bytes)
}

use super::delivery::consumer::{
    BorrowedReplay, ValidatedCursorCut, VerifiedCursor, VerifiedDeliverySource,
};
use super::delivery::{AdmissionOutcome, CompleteStateItem};
use crate::committed_authority::CommittedView;
use tilo_relay::delivery_v3::{
    auth::VerifiedSession,
    commit::{CommitRef, ConsumerId},
    cursor::CursorToken,
};
impl SessionOwner {
    pub(in crate::owner) fn validate_data_turn(
        &self,
        turn: &OwnerResourceTurn,
        operation: Option<OwnerDataOperation>,
    ) -> DataResult<()> {
        let matches = self
            .resources
            .as_ref()
            .is_some_and(|resources| Arc::ptr_eq(&resources.retained, &turn.retained));
        if !matches
            || turn.used
            || turn.generation != self.aggregate.generation
            || operation.is_some_and(|op| op != turn.operation)
        {
            return Err(OwnerDataFailure::new(
                Failure::CurrentDenied,
                anyhow::anyhow!("resource turn owner, generation or operation mismatch"),
            ));
        }
        Ok(())
    }
    pub(in crate::owner) fn with_data_operation<T>(
        &mut self,
        turn: &mut OwnerResourceTurn,
        operation: OwnerDataOperation,
        callback: impl FnOnce(&mut Self) -> Result<T>,
    ) -> DataResult<T> {
        self.validate_data_turn(turn, Some(operation))?;
        let resources = self.resources.as_ref().expect("validated owner resources");
        let active = resources.active.clone();
        {
            let mut slot = active.lock().map_err(|_| {
                OwnerDataFailure::new(
                    Failure::Unavailable,
                    anyhow::anyhow!("resource scope poisoned"),
                )
            })?;
            if slot.is_some() {
                return Err(OwnerDataFailure::new(
                    Failure::Unavailable,
                    anyhow::anyhow!("nested owner resource turn"),
                ));
            }
            *slot = Some(turn.active.clone());
        }
        turn.used = true;
        let reset = ActiveReset(active);
        let result = callback(self).map_err(|e| self.classify_data_error(e));
        drop(reset);
        self.reconcile_pending_input_credits();
        result
    }
    pub(crate) fn data_source<'a>(
        &self,
        turn: &mut OwnerResourceTurn,
        view: &'a CommittedView,
        session: &'a VerifiedSession,
        now_ns: u64,
    ) -> DataResult<VerifiedDeliverySource<'a>> {
        self.validate_data_turn(turn, None)?;
        if matches!(turn.operation, OwnerDataOperation::PrivateContext { .. }) {
            return Err(OwnerDataFailure::new(
                Failure::CurrentDenied,
                anyhow::anyhow!("private context reservation is not a data operation"),
            ));
        }
        if !view
            .retained_serialized_credit()
            .is_some_and(|credit| Arc::ptr_eq(credit, &turn.retained))
        {
            return Err(OwnerDataFailure::new(
                Failure::Unavailable,
                anyhow::anyhow!("committed view has not acquired owner resource custody"),
            ));
        }
        self.delivery_source(view, session, now_ns)
            .map_err(|e| self.classify_data_error(e))
    }
    pub(crate) fn data_admission_receipt<'a>(
        &'a self,
        turn: &mut OwnerResourceTurn,
        source: &VerifiedDeliverySource<'_>,
        delivery: tilo_relay::delivery_v3::commit::DeliveryId,
        digest: tilo_relay::delivery_v3::commit::Digest,
    ) -> DataResult<Option<&'a tilo_relay::delivery_v3::commit::AdmissionRecord>> {
        self.validate_data_turn(turn, Some(OwnerDataOperation::ReceiptLookup))?;
        self.delivery_admission_receipt(source, delivery, digest)
            .map_err(|e| self.classify_data_error(e))
    }
    pub(crate) fn data_register_consumer(
        &mut self,
        turn: &mut OwnerResourceTurn,
        source: &VerifiedDeliverySource<'_>,
    ) -> DataResult<ConsumerId> {
        self.with_data_operation(turn, OwnerDataOperation::RegisterConsumer, |owner| {
            owner.register_delivery_consumer(source)
        })
    }
    pub(crate) fn data_admit(
        &mut self,
        turn: &mut OwnerResourceTurn,
        source: &VerifiedDeliverySource<'_>,
        item: CompleteStateItem<'_>,
        now: SystemTime,
    ) -> DataResult<AdmissionOutcome> {
        let length = item.canonical.len();
        self.with_data_operation(
            turn,
            OwnerDataOperation::State { input_len: length },
            |owner| {
                let pending_credit = owner.split_retained_bytes(length)?;
                let before = owner.peer.pending_count();
                let outcome = owner.admit_delivery(source, item, now)?;
                if owner.peer.pending_count() > before {
                    if let Some(credit) = pending_credit {
                        owner.store_pending_input_credit(credit);
                    }
                }
                Ok(outcome)
            },
        )
    }
    /// Admit a non-empty prefix of `items` under one turn reserved for their
    /// total length; consecutive new document items share one commit group
    /// (`SessionOwner::receive_delivery_group`).
    pub(crate) fn data_admit_group(
        &mut self,
        turn: &mut OwnerResourceTurn,
        source: &VerifiedDeliverySource<'_>,
        items: &[CompleteStateItem<'_>],
        now: SystemTime,
    ) -> DataResult<Vec<AdmissionOutcome>> {
        let length = match turn.operation {
            OwnerDataOperation::State { input_len } => input_len,
            _ => 0,
        };
        // Only the prefix covered by this turn's reservation is admitted here;
        // the caller reserves a later turn for the rest.
        let mut total = 0usize;
        let fitting = items
            .iter()
            .take_while(|item| {
                total = total.saturating_add(item.canonical.len());
                total <= length
            })
            .count();
        if fitting == 0
            || items[..fitting]
                .iter()
                .any(|item| item.canonical.is_empty())
        {
            return Err(OwnerDataFailure::new(
                Failure::InvalidNativeInput,
                anyhow::anyhow!("group input exceeds its reserved turn"),
            ));
        }
        let items = &items[..fitting];
        self.with_data_operation(
            turn,
            OwnerDataOperation::State { input_len: length },
            |owner| {
                let pending_credit = owner.split_retained_bytes(length)?;
                let before = owner.peer.pending_count();
                let outcome = owner.admit_delivery_group(source, items, now)?;
                if owner.peer.pending_count() > before {
                    if let Some(credit) = pending_credit {
                        owner.store_pending_input_credit(credit);
                    }
                }
                Ok(outcome)
            },
        )
    }
    pub(crate) fn data_verify_cursor<'a>(
        &self,
        turn: &mut OwnerResourceTurn,
        source: &VerifiedDeliverySource<'a>,
        token: &CursorToken,
    ) -> DataResult<VerifiedCursor<'a>> {
        self.validate_data_turn(turn, None)?;
        if !matches!(
            turn.operation,
            OwnerDataOperation::Ack | OwnerDataOperation::Replay
        ) {
            return Err(OwnerDataFailure::new(
                Failure::CurrentDenied,
                anyhow::anyhow!("cursor operation requires ACK or replay turn"),
            ));
        }
        self.verify_delivery_cursor(source, token)
            .map_err(|e| self.classify_data_error(e))
    }
    pub(crate) fn data_acknowledge(
        &mut self,
        turn: &mut OwnerResourceTurn,
        source: &VerifiedDeliverySource<'_>,
        cut: ValidatedCursorCut,
    ) -> DataResult<()> {
        self.with_data_operation(turn, OwnerDataOperation::Ack, |owner| {
            owner.acknowledge_delivery(source, cut)
        })
    }
    pub(crate) fn data_replay<'a>(
        &'a self,
        turn: &mut OwnerResourceTurn,
        source: &VerifiedDeliverySource<'_>,
        committed: &CommitRef,
    ) -> DataResult<BorrowedReplay<'a>> {
        self.validate_data_turn(turn, Some(OwnerDataOperation::Replay))?;
        self.borrowed_delivery_replay(source, committed)
            .map_err(|e| self.classify_data_error(e))
    }
    pub(crate) fn data_issue_cursor(
        &self,
        turn: &mut OwnerResourceTurn,
        source: &VerifiedDeliverySource<'_>,
        committed: CommitRef,
    ) -> DataResult<CursorToken> {
        self.validate_data_turn(turn, Some(OwnerDataOperation::Replay))?;
        self.issue_delivery_cursor(source, committed)
            .map_err(|e| self.classify_data_error(e))
    }
    /// Test observation: the committed aggregate's encoded length.
    #[cfg(test)]
    pub(crate) fn aggregate_encoded_len(&self) -> usize {
        self.aggregate.encoded_payload_len().unwrap()
    }
    /// Test observation: bytes currently held as retained serialized credit.
    #[cfg(test)]
    pub(crate) fn accounted_retained_bytes(&self) -> usize {
        self.resources
            .as_ref()
            .map_or(0, |r| r.retained.bytes.load(Ordering::Relaxed))
    }
    /// Test observation: serialized bytes the owner actually retains now
    /// (aggregate buffers, publication pool payloads and live artifact).
    #[cfg(test)]
    pub(crate) fn actual_retained_bytes(&self) -> usize {
        let live = self.live_publication_transition.as_ref().map_or(0, |live| {
            live.room.capacity()
                + live.messages.len() * size_of::<(Vec<u8>, Retention)>()
                + live
                    .messages
                    .iter()
                    .map(|(p, _)| p.capacity())
                    .sum::<usize>()
        });
        retained_capacity(&self.aggregate).unwrap() + live
    }
    pub(crate) fn retained_serialized_credit(&self) -> Option<Arc<RetainedSerializedBytes>> {
        self.resources
            .as_ref()
            .map(|resources| resources.retained.clone())
    }
    /// Keep the host's not-yet-scoped commit paths (relay, collaboration,
    /// workers, descriptor refresh) working after the ingress pool attaches:
    /// outside a turn they keep the pre-attachment aggregate bound and stage
    /// no turn credit. Data routes still commit inside their reserved turns.
    pub(crate) fn permit_unscoped_legacy_commits(&mut self) -> Result<()> {
        self.resources
            .as_mut()
            .context("owner resources are not attached")?
            .unscoped_legacy = true;
        Ok(())
    }
    pub(super) fn serialized_commit_cap(&self) -> Result<usize> {
        match &self.resources {
            None => Ok(crate::store::MAX_AGGREGATE_BYTES),
            Some(resources) => resources
                .active
                .lock()
                .map_err(|_| anyhow::anyhow!("resource scope poisoned"))?
                .as_ref()
                .map(|active| active.cap)
                .or(resources
                    .unscoped_legacy
                    .then_some(crate::store::MAX_AGGREGATE_BYTES))
                .ok_or_else(|| {
                    delivery::error::failure(
                        Failure::Unavailable,
                        anyhow::anyhow!("attached owner mutation requires a resource turn"),
                    )
                }),
        }
    }
    fn split_retained_bytes(&self, bytes: usize) -> Result<Option<ByteCredit>> {
        let Some(resources) = &self.resources else {
            return Ok(None);
        };
        let Some(active) = resources
            .active
            .lock()
            .map_err(|_| anyhow::anyhow!("resource scope poisoned"))?
            .clone()
        else {
            // A permitted unscoped legacy commit stages no turn credit.
            anyhow::ensure!(
                resources.unscoped_legacy,
                "retained allocation outside resource turn"
            );
            return Ok(None);
        };
        let credit = active
            .retained_allowance
            .lock()
            .map_err(|_| anyhow::anyhow!("retained allowance poisoned"))?
            .split(bytes)
            .ok_or_else(capacity)?;
        Ok(Some(credit))
    }
    /// Keep an input's credit while the native deferred queue retains it.
    fn store_pending_input_credit(&self, credit: ByteCredit) {
        self.resources
            .as_ref()
            .expect("split owner retained credit")
            .retained
            .push_pending(credit);
    }
    /// Release deferred-input credits (oldest first) beyond the inputs the
    /// native queue still holds. Called after every owner turn and commit.
    pub(super) fn reconcile_pending_input_credits(&self) {
        let Some(resources) = &self.resources else {
            return;
        };
        let held = self.peer.pending_count();
        let mut pending = resources
            .retained
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        while pending.len() > held {
            pending.pop_front();
        }
    }
    fn stage_retained_credit(
        &self,
        bytes: usize,
        slot: impl FnOnce(&mut StagedCredits) -> &mut Option<ByteCredit>,
    ) -> Result<()> {
        if let Some(credit) = self.split_retained_bytes(bytes)? {
            let active = self
                .resources
                .as_ref()
                .expect("split owner retained credit")
                .active
                .lock()
                .map_err(|_| anyhow::anyhow!("resource scope poisoned"))?
                .clone()
                .context("retained candidate outside resource turn")?;
            let mut staged = active
                .staged
                .lock()
                .map_err(|_| anyhow::anyhow!("candidate credit scope poisoned"))?;
            let slot = slot(&mut staged);
            anyhow::ensure!(slot.is_none(), "unexpected additional retained candidate");
            *slot = Some(credit);
        }
        Ok(())
    }
    /// Stage the backing of the candidate's live publication artifact.
    pub(super) fn retain_live_publication_bytes(&self, bytes: usize) -> Result<()> {
        self.stage_retained_credit(bytes, |staged| &mut staged.live)
    }
    /// Stage the next aggregate's exact retained capacity (its raw buffers
    /// and every publication payload it holds). The previous generation's
    /// credit is released once the commit is durable and no pin holds it.
    pub(super) fn retain_commit_bytes(&self, next: &Aggregate) -> Result<()> {
        if self.resources.is_none() {
            return Ok(());
        }
        let bytes = retained_capacity(next)?;
        self.stage_retained_credit(bytes, |staged| &mut staged.generation)
    }
    /// After the durable barrier: the committed `next` aggregate replaces
    /// the previous generation's custody.
    pub(super) fn publish_retained_bytes(&self, next: &Aggregate) {
        let Some(resources) = &self.resources else {
            return;
        };
        let retained = &resources.retained;
        let active = resources
            .active
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        let Some(active) = active else {
            // Only a permitted unscoped legacy commit reaches here; it staged
            // no turn credit (see permit_unscoped_legacy_commits).
            assert!(
                resources.unscoped_legacy,
                "active serialized turn through durable barrier"
            );
            self.rebase_generation_unscoped(next);
            return;
        };
        let mut staged = active
            .staged
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(credit) = staged.generation.take() {
            let next = Arc::new(AccountedCredit::new(credit, &retained.bytes));
            let previous = std::mem::replace(
                &mut *retained
                    .generation
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()),
                next,
            );
            drop(previous);
        }
        let live = staged
            .live
            .take()
            .map(|credit| AccountedCredit::new(credit, &retained.bytes));
        *retained
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = live;
    }
    /// Unscoped legacy commits have no turn allowance. Resize the generation
    /// credit to the committed aggregate on a best-effort basis: shrinking
    /// always succeeds; growth that the pool cannot fund right now is left
    /// to the next scoped commit, which replaces the credit exactly.
    fn rebase_generation_unscoped(&self, next: &Aggregate) {
        let Some(resources) = &self.resources else {
            return;
        };
        let Ok(bytes) = retained_capacity(next) else {
            return;
        };
        let retained = &resources.retained;
        let mut generation = retained
            .generation
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(sole) = Arc::get_mut(&mut generation) {
            let held = sole.bytes();
            if bytes < held {
                sole.shrink(bytes);
            } else if bytes > held {
                match resources.budget.try_acquire_bulk(bytes - held) {
                    Some(extra) => sole.grow(extra),
                    None => tracing::debug!(
                        held,
                        bytes,
                        "unscoped commit growth not yet charged to owner custody"
                    ),
                }
            }
        } else if let Some(credit) = resources.budget.try_acquire_bulk(bytes) {
            // A pin still holds the previous generation; it keeps its own.
            *generation = Arc::new(AccountedCredit::new(credit, &retained.bytes));
        }
    }
    /// The installed live artifact takes the backing staged by its commit.
    pub(super) fn take_live_publication_credit(&self) -> Option<Arc<AccountedCredit>> {
        self.resources
            .as_ref()?
            .retained
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
            .map(Arc::new)
    }
}

#[path = "resources/scopes.rs"]
mod scopes;
pub(crate) use scopes::{OwnerOperation, OwnerScope};

#[path = "resources/publication_pin.rs"]
mod publication_pin;
pub(crate) use publication_pin::OwnedPublicationPin;

#[path = "resources/staged.rs"]
mod staged;

#[path = "resources/snapshot.rs"]
mod snapshot;
