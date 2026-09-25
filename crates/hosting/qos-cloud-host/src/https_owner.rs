//! The actual Cloud Store is the sole current authority for this component.
//! This is not an ingress installation or native allocation qualification.
use crate::{StatePeer, Store};
use anyhow::{Context, Result, ensure};
use atomic_file::FileIdentity;
use qos_action_broker::peer_actions::{OwnerPolicyClock, PolicyOrigin, PolicyTime};
use std::{
    fs::File,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

mod held;

pub(crate) enum Current<T> {
    Current(T),
    Busy,
    Denied,
}
#[derive(Clone)]
pub(crate) struct CloudStoreSource(Arc<Source>);
struct Source {
    root: PathBuf,
    machine: String,
    room: String,
    authority: qos_user_identity::DeviceCert,
    directory: File,
    /// Current store incarnation; advanced only by this owner's own rotation.
    incarnation: std::sync::RwLock<[u8; 16]>,
    last_generation: AtomicU64,
    denied: AtomicBool,
    clock: OwnerPolicyClock,
    policy_origin: PolicyOrigin,
    /// Validated decode of the unchanged aggregate between exclusive turns.
    retained: std::sync::Mutex<Option<Retained>>,
    /// Shared ingress pool that charges the retained decode, once known.
    retained_pool: std::sync::OnceLock<qos_session_peer::ingress::HostBytes>,
    /// Validated decode captured by `open_existing_store` itself (the agent
    /// owner's Store handles), reused by later opens of the identical files.
    opened: std::sync::Mutex<Option<OpenedRetained>>,
    #[cfg(test)]
    open_decodes: AtomicU64,
    #[cfg(test)]
    turns: AtomicU64,
    #[cfg(test)]
    decodes: AtomicU64,
}
/// The last turn's validated State and committed peer, reused only while the
/// locked aggregate (`state.json` and `state.journal`) is the identical files
/// (see `access::RetainedState`).
struct Retained {
    state: crate::delivery::access::RetainedState,
    peer: Option<CachedPeer>,
    _credit: qos_session_peer::ingress::ByteCredit,
}
/// A validated decode captured when `open_existing_store` had to read the
/// aggregate, with the clean observation of the exact files it came from.
struct OpenedRetained {
    aggregate: crate::storage::Aggregate,
    state: crate::State,
    tip: Option<crate::storage::Tip>,
    peer: Option<CachedPeer>,
    _credit: qos_session_peer::ingress::ByteCredit,
}
/// A committed peer restored at `at_ns`; reusable only for the same Store
/// generation and before any roster/authority/revocation time boundary.
struct CachedPeer {
    peer: StatePeer,
    generation: u64,
    at_ns: u64,
    until_ns: u64,
}
/// Longest interval a restored committed peer is reused without restoring.
const PEER_REUSE_NS: u64 = 30_000_000_000;
fn peer_reuse_until(state: &crate::State, at_ns: u64) -> u64 {
    let mut until = at_ns.saturating_add(PEER_REUSE_NS);
    if let Some(bound) = &state.bound {
        let mut bound_at = |instant: Option<u64>| {
            if let Some(instant) = instant.filter(|instant| *instant > at_ns) {
                until = until.min(instant);
            }
        };
        for certificate in bound.roster.iter().chain(std::iter::once(&bound.authority)) {
            bound_at(certificate.expires_at_ns);
            bound_at(Some(certificate.issued_at_ns));
        }
        for revocation in &bound.revocations {
            bound_at(Some(revocation.revoked_at_ns));
        }
    }
    until
}
pub(crate) struct CloudStoreTurn {
    source: CloudStoreSource,
    store: Store,
    peer: StatePeer,
    now_ns: u64,
    policy_time: PolicyTime,
    delivery_work: Option<transaction::DeliveryWork>,
}
impl CloudStoreSource {
    /// Bind only the real enrolled Store. This does not attest resource limits,
    /// handler installation or transport liveness; no descriptor can be minted here.
    pub(crate) fn bind_with_clock(store: &Store, clock: OwnerPolicyClock) -> Result<Self> {
        clock.now_ns()?;
        store.ensure_reconciled()?;
        ensure!(
            store.state.version == crate::delivery::VERSION,
            "explicit delivery selection required"
        );
        let delivery = store
            .state
            .delivery
            .as_ref()
            .context("delivery state missing")?;
        let bound = store
            .state
            .bound
            .as_ref()
            .context("enrolled Store required")?;
        ensure!(
            bound.authority.device_public_key == store.key().verifying_key().to_bytes(),
            "Store host mismatch"
        );
        Ok(Self(Arc::new(Source {
            root: store.root.clone(),
            machine: store.state.machine_id.clone(),
            room: store.state.room.clone(),
            authority: bound.authority.clone(),
            directory: store.directory.try_clone()?,
            incarnation: std::sync::RwLock::new(delivery.store_generation()),
            last_generation: AtomicU64::new(store.state.generation),
            denied: AtomicBool::new(false),
            policy_origin: clock.new_origin(),
            clock,
            retained: std::sync::Mutex::new(None),
            retained_pool: std::sync::OnceLock::new(),
            opened: std::sync::Mutex::new(None),
            #[cfg(test)]
            open_decodes: AtomicU64::new(0),
            #[cfg(test)]
            turns: AtomicU64::new(0),
            #[cfg(test)]
            decodes: AtomicU64::new(0),
        })))
    }
    #[cfg(test)]
    pub(crate) fn bind(store: &Store) -> Result<Self> {
        Self::bind_with_clock(store, OwnerPolicyClock::new()?)
    }
    pub(crate) fn with_current<R>(&self, action: impl FnOnce(&CloudStoreTurn) -> R) -> Current<R> {
        self.with_current_mut(|turn| action(turn))
    }
    fn with_current_mut<R>(&self, action: impl FnOnce(&mut CloudStoreTurn) -> R) -> Current<R> {
        self.with_current_admitting(&mut || true, action)
    }
    /// `admit_decode` runs under the lock only when the aggregate must be read
    /// and decoded again; refusing it releases the lock and reports Busy.
    pub(super) fn with_current_admitting<R>(
        &self,
        admit_decode: &mut dyn FnMut() -> bool,
        action: impl FnOnce(&mut CloudStoreTurn) -> R,
    ) -> Current<R> {
        if self.0.denied.load(Ordering::Acquire) {
            return Current::Denied;
        }
        let retained = self.take_retained();
        let (mut state, peer, credit) = match retained {
            Some(retained) => (Some(retained.state), retained.peer, Some(retained._credit)),
            None => (None, None, None),
        };
        let (store, opened) = match crate::delivery::access::try_open_retained(
            &self.0.root,
            &self.0.machine,
            &mut state,
            admit_decode,
        ) {
            Ok(opened) => opened,
            Err(crate::delivery::access::AccessError::Busy) => {
                // Another Cloud component holds the lock; keep the retained
                // decode for our next turn, it is revalidated then.
                if let (Some(state), Some(credit)) = (state, credit) {
                    self.put_retained(Retained {
                        state,
                        peer,
                        _credit: credit,
                    });
                }
                return Current::Busy;
            }
            Err(crate::delivery::access::AccessError::Denied(error)) => {
                tracing::warn!(%error, "Cloud Store turn open denied");
                return self.deny();
            }
        };
        drop(state);
        #[cfg(test)]
        {
            self.0.turns.fetch_add(1, Ordering::Relaxed);
            if !opened.reused {
                self.0.decodes.fetch_add(1, Ordering::Relaxed);
            }
        }
        let peer = if opened.reused { peer } else { None };
        let mut turn = match self.turn_reusing(store, peer) {
            Ok(turn) => turn,
            Err(error) => {
                tracing::warn!(%error, "Cloud Store turn validation denied");
                return self.deny();
            }
        };
        let result = action(&mut turn);
        let CloudStoreTurn {
            store,
            peer,
            now_ns,
            delivery_work,
            ..
        } = turn;
        let generation = store.state.generation;
        let until = peer_reuse_until(&store.state, now_ns);
        let length = opened.length();
        if let Some(state) = opened.retain(store) {
            // Decoded state and the restored peer (plus the journal diff
            // base, when retained), charged while retained.
            let base = state.base_len() as u64;
            let credit = credit.or_else(|| {
                self.0.retained_pool.get().and_then(|pool| {
                    length
                        .checked_mul(2)
                        .and_then(|bytes| bytes.checked_add(base))
                        .and_then(|bytes| bytes.checked_add(1024 * 1024))
                        .and_then(|bytes| usize::try_from(bytes).ok())
                        .and_then(|bytes| pool.try_acquire_bulk(bytes))
                })
            });
            if let Some(credit) = credit.filter(|_| !self.0.denied.load(Ordering::Acquire)) {
                self.put_retained(Retained {
                    state,
                    peer: Some(CachedPeer {
                        peer,
                        generation,
                        at_ns: now_ns,
                        until_ns: until,
                    }),
                    _credit: credit,
                });
            }
        }
        drop(delivery_work);
        Current::Current(result)
    }
    fn take_retained(&self) -> Option<Retained> {
        self.0
            .retained
            .lock()
            .ok()
            .and_then(|mut retained| retained.take())
    }
    fn put_retained(&self, value: Retained) {
        if let Ok(mut retained) = self.0.retained.lock() {
            *retained = Some(value);
        }
    }
    /// Charge future retained decodes to the shared ingress pool.
    pub(crate) fn bind_retained_pool(&self, pool: &qos_session_peer::ingress::HostBytes) {
        let _ = self.0.retained_pool.set(pool.clone());
    }
    /// Whether the next turn can reuse the retained decode (a hint only; the
    /// turn itself re-proves file identity under the Store lock).
    pub(crate) fn retains_current(&self) -> bool {
        self.0.retained.lock().ok().is_some_and(|retained| {
            retained
                .as_ref()
                .is_some_and(|r| r.state.matches(&self.0.directory))
        })
    }
    /// Drop the retained decode and its pool charge.
    #[cfg(test)]
    pub(crate) fn release_retained(&self) {
        drop(self.take_retained());
    }
    #[cfg(test)]
    pub(crate) fn decode_count(&self) -> u64 {
        self.0.decodes.load(Ordering::Relaxed)
    }
    /// The source's own trusted policy clock, sampled without the Store lock
    /// (the same clock every turn uses). Fails once the source is retired.
    pub(crate) fn policy_now_ns(&self) -> Result<u64> {
        ensure!(!self.0.denied.load(Ordering::Acquire), "source denied");
        Ok(self.0.clock.now_ns()?)
    }
    pub(crate) fn owns_turn(&self, turn: &CloudStoreTurn) -> bool {
        Arc::ptr_eq(&self.0, &turn.source.0)
    }
    pub(crate) fn validate_store(&self, store: &Store, time: &PolicyTime) -> Result<()> {
        self.checked_peer(store, time).map(|_| ())
    }
    pub(crate) fn open_existing_store(&self, time: &PolicyTime) -> Result<Store> {
        let began = std::time::Instant::now();
        let mut open_detail = (0u64, 0u64, 0u64);
        let opened = (|| {
            self.validate_time(time)?;
            // Acquire only the existing original Store sentinel. This path cannot
            // provision, migrate, or bootstrap a replacement before validation.
            // Under that lock, an identical aggregate reuses a copy of the
            // retained validated decode instead of re-reading and decoding it.
            let now = time.now_ns()?;
            let mut peer_current = false;
            let (mut store, reused) =
                crate::delivery::access::open_reusing(&self.0.root, &self.0.machine, |aggregate| {
                    let current = |peer: &Option<CachedPeer>, generation: u64| {
                        peer.as_ref().is_some_and(|peer| {
                            peer.generation == generation
                                && peer.at_ns <= now
                                && now < peer.until_ns
                        })
                    };
                    if let Some(retained) = self.0.retained.lock().ok().and_then(|retained| {
                        let retained = retained.as_ref()?;
                        retained.state.same_aggregate(aggregate).then(|| {
                            (
                                current(&retained.peer, retained.state.generation()),
                                retained.state.state_copy(),
                            )
                        })
                    }) {
                        peer_current = retained.0;
                        return Some(retained.1);
                    }
                    let opened = self.0.opened.lock().ok()?;
                    let opened = opened.as_ref()?;
                    if !opened.aggregate.same(aggregate) {
                        return None;
                    }
                    peer_current = current(&opened.peer, opened.state.generation);
                    Some((opened.state.clone(), opened.tip.clone()))
                })
                .map_err(|error| match error {
                    crate::delivery::access::AccessError::Busy => anyhow::anyhow!("owner busy"),
                    crate::delivery::access::AccessError::Denied(error) => error,
                })?;
            open_detail = (
                u64::from(reused),
                u64::from(peer_current),
                qos_action_broker::agent_timing::us(began),
            );
            if reused && peer_current {
                // The retained peer was restored from this exact validated
                // state within its time window; only the cheap current
                // owner/time/identity checks need repeating.
                self.validate_time(time)?;
                store.ensure_reconciled()?;
                self.checked_state(&store.state, &store.directory, time)?;
            } else {
                let (peer, at_ns) = self.checked_peer(&store, time)?;
                if reused {
                    self.refresh_opened_peer(&store, peer, at_ns);
                } else {
                    self.retain_opened(&store, peer, at_ns);
                }
            }
            store.bind_policy_time(time.clone())?;
            Ok(store)
        })();
        if let Err(error) = &opened {
            tracing::warn!(%error, "Cloud Store existing open denied");
            self.retire();
        }
        qos_action_broker::agent_timing::slow_store(
            "store_open",
            began,
            &[
                ("reused", open_detail.0),
                ("peer_current", open_detail.1),
                ("read_us", open_detail.2),
            ],
        );
        opened
    }
    /// Keep a copy of a freshly read and validated decode (and the peer it
    /// restored) for later `open_existing_store` calls over the identical
    /// files: the agent owner then pays one decode per aggregate change, not
    /// per open. Charged to the shared ingress pool; skipped without one.
    fn retain_opened(&self, store: &Store, peer: StatePeer, at_ns: u64) {
        #[cfg(test)]
        self.0.open_decodes.fetch_add(1, Ordering::Relaxed);
        let Some(pool) = self.0.retained_pool.get() else {
            return;
        };
        let tip = store.tip.borrow().clone();
        let Some(observed) = tip.as_ref().and_then(|tip| {
            let directory =
                atomic_file::StorageDirectory::from_file(store.directory.try_clone().ok()?).ok()?;
            crate::storage::Aggregate::observe_tip(&directory, tip)
        }) else {
            return;
        };
        let bytes = observed
            .length()
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(tip.as_ref().map_or(0, |tip| tip.len() as u64)))
            .and_then(|bytes| bytes.checked_add(1024 * 1024))
            .and_then(|bytes| usize::try_from(bytes).ok());
        // Release the previous copy before charging its replacement.
        drop(
            self.0
                .opened
                .lock()
                .ok()
                .and_then(|mut opened| opened.take()),
        );
        let Some(credit) = bytes.and_then(|bytes| pool.try_acquire_bulk(bytes)) else {
            return;
        };
        if self.0.denied.load(Ordering::Acquire) {
            return;
        }
        let generation = store.state.generation;
        let until_ns = peer_reuse_until(&store.state, at_ns);
        if let Ok(mut opened) = self.0.opened.lock() {
            *opened = Some(OpenedRetained {
                aggregate: observed,
                state: store.state.clone(),
                tip,
                peer: Some(CachedPeer {
                    peer,
                    generation,
                    at_ns,
                    until_ns,
                }),
                _credit: credit,
            });
        }
    }
    /// A reused decode whose cached peer had lapsed: keep the peer just
    /// restored from that same validated generation.
    fn refresh_opened_peer(&self, store: &Store, peer: StatePeer, at_ns: u64) {
        if let Ok(mut opened) = self.0.opened.lock() {
            if let Some(opened) = opened
                .as_mut()
                .filter(|opened| opened.state.generation == store.state.generation)
            {
                opened.peer = Some(CachedPeer {
                    until_ns: peer_reuse_until(&store.state, at_ns),
                    peer,
                    generation: store.state.generation,
                    at_ns,
                });
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn open_decode_count(&self) -> u64 {
        self.0.open_decodes.load(Ordering::Relaxed)
    }
    fn validate_time(&self, time: &PolicyTime) -> Result<u64> {
        self.checked_time(time).map_err(Into::into)
    }
    fn checked_time(&self, time: &PolicyTime) -> std::result::Result<u64, CloudDeliveryError> {
        if self.0.denied.load(Ordering::Acquire)
            || !self.0.policy_origin.owns(time)
            || !time.belongs_to(&self.0.clock)
        {
            return Err(CloudDeliveryError::Denied);
        }
        time.now_ns().map_err(|_| CloudDeliveryError::Denied)
    }
    /// Adopt the incarnation this owner itself just rotated to under its held
    /// Store lock. Any other incarnation change still denies the source.
    fn accept_rotation(&self, from: [u8; 16], to: [u8; 16]) -> bool {
        match self.0.incarnation.write() {
            Ok(mut incarnation) if *incarnation == from => {
                *incarnation = to;
                true
            }
            _ => false,
        }
    }
    #[track_caller]
    pub(crate) fn retire(&self) {
        self.0.policy_origin.invalidate();
        if !self.0.denied.swap(true, Ordering::AcqRel) {
            // Content-free: which check retired this source (the first one).
            tracing::info!(site = %std::panic::Location::caller(), "Cloud Store source retired");
        }
        drop(self.take_retained());
        drop(
            self.0
                .opened
                .lock()
                .ok()
                .and_then(|mut opened| opened.take()),
        );
    }
    #[track_caller]
    fn deny<R>(&self) -> Current<R> {
        // This source never resumes after unknown/corrupt state. A fresh actual
        // owner binding and fresh service/session registries are required.
        self.retire();
        Current::Denied
    }
    fn checked_peer(&self, store: &Store, time: &PolicyTime) -> Result<(StatePeer, u64)> {
        self.checked_peer_reusing(store, time, None)
    }
    fn checked_peer_reusing(
        &self,
        store: &Store,
        time: &PolicyTime,
        cached: Option<CachedPeer>,
    ) -> Result<(StatePeer, u64)> {
        let checked = (|| {
            self.validate_time(time)?;
            store.ensure_reconciled()?;
            let now_ns = self.checked_state(&store.state, &store.directory, time)?;
            let peer = match cached {
                Some(cached)
                    if cached.generation == store.state.generation
                        && cached.at_ns <= now_ns
                        && now_ns < cached.until_ns =>
                {
                    cached.peer
                }
                _ => crate::delivery::committed_peer(&store.state, now_ns)?,
            };
            ensure!(
                peer.authority() == &self.0.authority,
                "current authority changed"
            );
            self.validate_time(time)?;
            Ok((peer, now_ns))
        })();
        if let Err(error) = &checked {
            tracing::warn!(%error, "Cloud Store peer check denied");
            self.retire();
        }
        checked
    }
    /// Shared configured-owner checks only. The caller must also restore and
    /// validate the actual committed peer using its concrete storage mode.
    fn checked_state(
        &self,
        state: &crate::State,
        directory: &File,
        time: &PolicyTime,
    ) -> std::result::Result<u64, CloudDeliveryError> {
        let now_ns = self.checked_time(time)?;
        if state.version != crate::delivery::VERSION {
            return Err(CloudDeliveryError::Denied);
        }
        let delivery = state.delivery.as_ref().ok_or(CloudDeliveryError::Denied)?;
        let bound = state.bound.as_ref().ok_or(CloudDeliveryError::Denied)?;
        if !(FileIdentity::of(directory).map_err(|_| CloudDeliveryError::Denied)?
            == FileIdentity::of(&self.0.directory).map_err(|_| CloudDeliveryError::Denied)?
            && state.room == self.0.room
            && state.machine_id == self.0.machine
            && bound.authority == self.0.authority
            && bound.root == self.0.authority.user_public_key
            && ed25519_dalek::SigningKey::from_bytes(&state.host_secret)
                .verifying_key()
                .to_bytes()
                == self.0.authority.device_public_key)
        {
            tracing::warn!("Cloud Store identity check denied");
            return Err(CloudDeliveryError::Denied);
        }
        if !self
            .0
            .incarnation
            .read()
            .is_ok_and(|incarnation| delivery.store_generation() == *incarnation)
        {
            tracing::warn!(
                generation = state.generation,
                "Cloud Store incarnation check denied"
            );
            return Err(CloudDeliveryError::Denied);
        }
        let previous = self
            .0
            .last_generation
            .fetch_max(state.generation, Ordering::AcqRel);
        if state.generation < previous || bound.authority.is_expired_at(now_ns) {
            tracing::warn!(
                generation = state.generation,
                previous,
                "Cloud Store generation or authority check denied"
            );
            return Err(CloudDeliveryError::Denied);
        }
        Ok(now_ns)
    }
    fn turn_reusing(&self, mut store: Store, cached: Option<CachedPeer>) -> Result<CloudStoreTurn> {
        let policy_time = self.0.policy_origin.capture()?;
        let (peer, now_ns) = self.checked_peer_reusing(&store, &policy_time, cached)?;
        store.bind_policy_time(policy_time.clone())?;
        Ok(CloudStoreTurn {
            source: self.clone(),
            store,
            peer,
            now_ns,
            policy_time,
            delivery_work: None,
        })
    }
}
impl CloudStoreTurn {
    pub(crate) fn policy_time(&self) -> Result<PolicyTime> {
        self.fresh_now_ns()?;
        Ok(self.policy_time.clone())
    }
    pub(crate) fn peer(&self) -> &StatePeer {
        &self.peer
    }
    pub(crate) fn generation(&self) -> u64 {
        self.store.state.generation
    }
    pub(crate) fn policy_lineage(&self) -> u64 {
        self.store
            .state
            .delivery
            .as_ref()
            .expect("validated selection")
            .policy_lineage()
    }
    pub(crate) fn now_ns(&self) -> u64 {
        self.now_ns
    }
    /// Resample the same trusted clock while this exact Store turn remains held.
    /// Admission context keeps its original timestamp; final disclosure must
    /// additionally intersect this current time immediately before handoff.
    pub(crate) fn fresh_now_ns(&self) -> Result<u64> {
        ensure!(
            !self.source.0.denied.load(Ordering::Acquire),
            "source denied"
        );
        ensure!(
            self.source.0.policy_origin.owns(&self.policy_time)
                && self.policy_time.belongs_to(&self.source.0.clock),
            "source time origin changed"
        );
        Ok(self.policy_time.now_ns()?)
    }
    pub(crate) fn workspace(&self) -> u64 {
        qos_relay::room_workspace_id(&self.store.state.room).expect("validated room")
    }
    pub(crate) fn continuity(&self) -> u64 {
        // A Source is permanently denied after any non-Busy observation failure;
        // it has one uninterrupted lifetime and never silently recovers.
        1
    }
    pub(crate) fn directory(&self) -> &File {
        &self.store.directory
    }
    /// Delivery budget use; a read-only observation of this turn's state.
    pub(crate) fn delivery_pressure(&self) -> Result<crate::delivery::retention::Pressure> {
        self.store.delivery_pressure(self.now_ns)
    }
    /// Commit due delivery retention (receipt release, or a snapshot + suffix
    /// store rotation) under this Maintenance turn. A rotation is adopted by
    /// this source only because it is this turn's own verified commit.
    pub(crate) fn retain_delivery(
        &mut self,
        force_rotation: bool,
    ) -> Result<Option<crate::delivery::retention::RetentionReport>> {
        self.require_delivery_operation(DeliveryOperation::Maintenance)
            .map_err(anyhow::Error::from)?;
        let now = self.fresh_now_ns()?;
        let before = self
            .store
            .state
            .delivery
            .as_ref()
            .context("delivery state missing")?
            .store_generation();
        let report = match self.store.retain_delivery_with(now, force_rotation) {
            Ok(report) => report,
            Err(error) => {
                if let Some(failure) = error.downcast_ref::<crate::StoreCommitFailure>() {
                    let failure = *failure;
                    self.source.retire();
                    return Err(CloudDeliveryError::OwnerUncertain(failure).into());
                }
                return Err(error);
            }
        };
        if let Some(report) = &report {
            if let Some(rotated) = report.rotated {
                ensure!(
                    self.source.accept_rotation(before, rotated),
                    "store incarnation changed concurrently"
                );
            }
            self.refresh_committed()?;
        }
        Ok(report)
    }
}
mod agent_live;
mod auth;
mod config;
mod live;
mod media;
mod private;
pub(crate) mod serve;
mod service;
mod snapshot;
mod state;
mod transaction;
pub(crate) use service::agent_rejection_code;
pub(crate) use transaction::{CloudDeliveryError, CloudPinExpectation, DeliveryOperation};
mod data;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) async fn prepared_transport_test() -> (
    tempfile::TempDir,
    qos_session_peer::ingress::PreparedTransport,
) {
    data::tests::prepared_transport().await
}
