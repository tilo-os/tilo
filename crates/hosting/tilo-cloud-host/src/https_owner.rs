//! The actual Cloud Store is the sole current authority for this component.
//! This is not an ingress installation or native allocation qualification.
use crate::{StatePeer, Store};
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tilo_action_broker::peer_actions::{OwnerPolicyClock, PolicyOrigin, PolicyTime};
use tilo_files::native::atomic::FileIdentity;

pub(crate) enum Current<T> {
    #[expect(
        clippy::enum_variant_names,
        reason = "matched as Current::Current throughout the owner; renaming would obscure the call sites"
    )]
    Current(T),
    Busy,
    Denied,
}
#[derive(Clone)]
pub(crate) struct CloudStoreSource(Arc<Source>);
struct Source {
    #[cfg(test)]
    root: std::path::PathBuf,
    machine: String,
    room: String,
    authority: tilo_user_identity::DeviceCert,
    directory: File,
    /// The long-lived Store every turn borrows.
    owner: Arc<crate::owner_cell::Installed>,
    /// Current store incarnation; advanced only by this owner's own rotation.
    incarnation: std::sync::RwLock<[u8; 16]>,
    last_generation: AtomicU64,
    denied: AtomicBool,
    clock: OwnerPolicyClock,
    policy_origin: PolicyOrigin,
    /// The committed peer the last turn restored, reused while the Store
    /// generation and every identity time boundary are unchanged.
    peer: std::sync::Mutex<Option<CachedPeer>>,
    #[cfg(test)]
    turns: AtomicU64,
}
/// A committed peer restored at `at_ns`; reusable only for the same Store
/// generation and before any roster/authority/revocation time boundary.
struct CachedPeer {
    peer: StatePeer,
    generation: u64,
    at_ns: u64,
    until_ns: u64,
}
impl CachedPeer {
    fn current(&self, generation: u64, now_ns: u64) -> bool {
        self.generation == generation && self.at_ns <= now_ns && now_ns < self.until_ns
    }
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
    store: crate::StoreGuard,
    peer: StatePeer,
    now_ns: u64,
    policy_time: PolicyTime,
    delivery_work: Option<transaction::DeliveryWork>,
}
impl CloudStoreSource {
    /// Bind the real enrolled Store and keep it in memory as this root's
    /// long-lived owner (or join the one already installed). This does not
    /// attest resource limits, handler installation or transport liveness; no
    /// descriptor can be minted here.
    pub(crate) fn bind_with_clock(
        store: impl Into<crate::StoreGuard>,
        clock: OwnerPolicyClock,
    ) -> Result<Self> {
        let mut store = store.into();
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
        let incarnation = delivery.store_generation();
        #[cfg(test)]
        let root = store.root.clone();
        let (machine, room, authority, directory, generation) = (
            store.state.machine_id.clone(),
            store.state.room.clone(),
            bound.authority.clone(),
            store.directory.try_clone()?,
            store.state.generation,
        );
        let owner = store.long_lived()?;
        Ok(Self(Arc::new(Source {
            #[cfg(test)]
            root,
            machine,
            room,
            authority,
            directory,
            owner,
            incarnation: std::sync::RwLock::new(incarnation),
            last_generation: AtomicU64::new(generation),
            denied: AtomicBool::new(false),
            policy_origin: clock.new_origin(),
            clock,
            peer: std::sync::Mutex::new(None),
            #[cfg(test)]
            turns: AtomicU64::new(0),
        })))
    }
    #[cfg(test)]
    pub(crate) fn bind(store: impl Into<crate::StoreGuard>) -> Result<Self> {
        Self::bind_with_clock(store, OwnerPolicyClock::new()?)
    }
    pub(crate) fn with_current<R>(&self, action: impl FnOnce(&CloudStoreTurn) -> R) -> Current<R> {
        self.with_current_mut(|turn| action(turn))
    }
    /// One turn over the long-lived Store. Another turn holding it is `Busy`.
    pub(super) fn with_current_mut<R>(
        &self,
        action: impl FnOnce(&mut CloudStoreTurn) -> R,
    ) -> Current<R> {
        if self.0.denied.load(Ordering::Acquire) {
            return Current::Denied;
        }
        let store = match self.0.owner.lend(false) {
            Ok(store) => store,
            Err(crate::delivery::access::AccessError::Busy) => return Current::Busy,
            Err(crate::delivery::access::AccessError::Denied(error)) => {
                tracing::warn!(%error, "Cloud Store turn open denied");
                return self.deny();
            }
        };
        #[cfg(test)]
        self.0.turns.fetch_add(1, Ordering::Relaxed);
        let cached = self.0.peer.lock().ok().and_then(|mut peer| peer.take());
        let mut turn = match self.turn_reusing(store, cached) {
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
        if store.failed_commit.get().is_none() && !self.0.denied.load(Ordering::Acquire) {
            let cached = CachedPeer {
                until_ns: peer_reuse_until(&store.state, now_ns),
                generation: store.state.generation,
                at_ns: now_ns,
                peer,
            };
            if let Ok(mut slot) = self.0.peer.lock() {
                *slot = Some(cached);
            }
        }
        drop(store);
        drop(delivery_work);
        Current::Current(result)
    }
    /// The source's own trusted policy clock, sampled without the Store lock
    /// (the same clock every turn uses). Fails once the source is retired.
    pub(crate) fn policy_now_ns(&self) -> Result<u64> {
        ensure!(!self.0.denied.load(Ordering::Acquire), "source denied");
        Ok(self.0.clock.now_ns()?)
    }
    /// Whether grants may be issued (see `owner_cell::governance_reconciled`).
    pub(crate) fn governance_reconciled(&self) -> bool {
        self.0.owner.governance_reconciled()
    }
    pub(crate) fn owns_turn(&self, turn: &CloudStoreTurn) -> bool {
        Arc::ptr_eq(&self.0, &turn.source.0)
    }
    #[cfg(test)]
    pub(crate) fn validate_store(&self, store: &Store, time: &PolicyTime) -> Result<()> {
        self.checked_peer(store, time).map(|_| ())
    }
    /// The long-lived Store for the agent owner's own turn at `time`, after
    /// the same owner, identity and time checks as every source turn.
    pub(crate) fn open_existing_store(&self, time: &PolicyTime) -> Result<crate::StoreGuard> {
        let opened = (|| {
            self.validate_time(time)?;
            let mut store = self.0.owner.lend(true).map_err(|error| match error {
                crate::delivery::access::AccessError::Busy => anyhow::anyhow!("owner busy"),
                crate::delivery::access::AccessError::Denied(error) => error,
            })?;
            let now = time.now_ns()?;
            let cached = self
                .0
                .peer
                .lock()
                .ok()
                .and_then(|peer| {
                    peer.as_ref()
                        .map(|peer| peer.current(store.state.generation, now))
                })
                .unwrap_or(false);
            if cached {
                // The cached peer was restored from this exact generation
                // within its time window; only the cheap current
                // owner/time/identity checks need repeating.
                self.validate_time(time)?;
                store.ensure_reconciled()?;
                self.checked_state(&store.state, &store.directory, time)?;
            } else {
                let (peer, at_ns) = self.checked_peer(&store, time)?;
                if let Ok(mut slot) = self.0.peer.lock() {
                    *slot = Some(CachedPeer {
                        until_ns: peer_reuse_until(&store.state, at_ns),
                        generation: store.state.generation,
                        at_ns,
                        peer,
                    });
                }
            }
            store.bind_policy_time(time.clone())?;
            Ok(store)
        })();
        if let Err(error) = &opened {
            tracing::warn!(%error, "Cloud Store existing open denied");
            self.retire();
        }
        opened
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
        self.0.owner.retire();
        self.0.policy_origin.invalidate();
        if !self.0.denied.swap(true, Ordering::AcqRel) {
            // Content-free: which check retired this source (the first one).
            tracing::info!(site = %std::panic::Location::caller(), "Cloud Store source retired");
        }
        if let Ok(mut peer) = self.0.peer.lock() {
            peer.take();
        }
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
            && state.host_key().verifying_key().to_bytes() == self.0.authority.device_public_key)
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
    fn turn_reusing(
        &self,
        mut store: crate::StoreGuard,
        cached: Option<CachedPeer>,
    ) -> Result<CloudStoreTurn> {
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
/// One Cloud Store turn seen through the shared owner seam. The media policy is
/// derived from the committed peer only when a service asks for it, so live
/// permission observations never pay for it.
pub(crate) struct CloudOwnerTurn<'a> {
    turn: &'a CloudStoreTurn,
    policy: std::cell::OnceCell<tilo_workspace_host::ingress::RoomMediaPolicy>,
    now: u64,
}
impl<'a> CloudOwnerTurn<'a> {
    /// View `turn` at `now`, an instant the caller sampled from this turn's clock.
    pub(crate) fn at(turn: &'a CloudStoreTurn, now: u64) -> Self {
        Self {
            turn,
            policy: std::cell::OnceCell::new(),
            now,
        }
    }
}
impl tilo_workspace_host::owner_turn::OwnerTurn for CloudOwnerTurn<'_> {
    fn peer(&self) -> &StatePeer {
        self.turn.peer()
    }
    fn media_policy(&self) -> &tilo_workspace_host::ingress::RoomMediaPolicy {
        self.policy.get_or_init(|| {
            tilo_workspace_host::ingress::RoomMediaPolicy::from_committed(self.turn.peer())
        })
    }
    fn workspace(&self) -> u64 {
        self.turn.workspace()
    }
    fn continuity(&self) -> u64 {
        self.turn.continuity()
    }
    fn now_ns(&self) -> u64 {
        self.now
    }
    fn fresh_now_ns(&self) -> Option<u64> {
        self.turn.fresh_now_ns().ok()
    }
}
impl tilo_workspace_host::owner_turn::HostOwner for CloudStoreSource {
    type Turn<'a> = CloudOwnerTurn<'a>;
    fn with_current<R>(
        &self,
        action: impl FnOnce(&Self::Turn<'_>) -> R,
    ) -> tilo_workspace_host::owner_turn::Observed<R> {
        use tilo_workspace_host::owner_turn::Observed;
        match CloudStoreSource::with_current(self, |turn| {
            action(&CloudOwnerTurn::at(turn, turn.now_ns()))
        }) {
            Current::Current(value) => Observed::Current(value),
            Current::Busy => Observed::Busy,
            Current::Denied => Observed::Denied,
        }
    }
    fn now_ns(&self) -> Option<u64> {
        self.policy_now_ns().ok()
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
        tilo_relay::room_workspace_id(&self.store.state.room).expect("validated room")
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
    tilo_workspace_host::ingress::PreparedTransport,
) {
    data::tests::prepared_transport().await
}
