//! The owner's committed peer, independent of action or data transport health.
//! Reads retain the publication lock through authorization; no stale snapshot
//! can authorize after a completed owner turn. Mutations are synchronous only.
use crate::SessionOwner;
use anyhow::{Result, ensure};
use qos_peer_state::StatePeer;
use std::{
    panic::Location,
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard, TryLockError,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Readers observe with `try_lock`, so a long hold is contention for every
/// relay session. Holds at least this long are logged (content-free: kind,
/// acquiring source location and duration) to diagnose relay fence expiry.
const SLOW_HOLD: Duration = Duration::from_millis(50);
struct Held {
    since: Instant,
    site: &'static Location<'static>,
}
impl Held {
    #[track_caller]
    fn new() -> Self {
        Self {
            since: Instant::now(),
            site: Location::caller(),
        }
    }
    fn finish(&self, kind: &'static str) {
        let held = self.since.elapsed();
        if held >= SLOW_HOLD {
            tracing::debug!(
                kind,
                site = %self.site,
                held_ms = held.as_millis() as u64,
                "committed authority held"
            );
        }
    }
}

pub(crate) struct CommittedView {
    peer: StatePeer,
    committed_peer_identity: [u8; 32],
    _serialized_credit: Option<Arc<crate::owner::resources::RetainedSerializedBytes>>,
    generation: u64,
    continuity: u64,
    private_policy: Option<qos_action_broker::peer_actions::PrivatePolicyBasis>,
    policy_epoch: u64,
    policy_origin: Arc<AtomicBool>,
    time_origin: Option<Arc<qos_action_broker::peer_actions::PolicyOrigin>>,
    #[cfg(feature = "media")]
    media_policy: crate::room_media_policy::RoomMediaPolicy,
}
impl CommittedView {
    pub(crate) fn peer(&self) -> &StatePeer {
        &self.peer
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn retained_serialized_credit(
        &self,
    ) -> Option<&Arc<crate::owner::resources::RetainedSerializedBytes>> {
        self._serialized_credit.as_ref()
    }
    pub(crate) fn committed_peer_identity(&self) -> [u8; 32] {
        self.committed_peer_identity
    }
    pub(crate) fn continuity(&self) -> u64 {
        self.continuity
    }
}
enum State {
    Current(CommittedView),
    Updating,
    Incomplete { generation: u64, continuity: u64 },
    Denied,
}
struct Shared {
    #[cfg(test)]
    after_time_retirement: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    policy_time: Option<private_policy::OwnerTime>,
    policy_origin: Arc<AtomicBool>,
    policy_epoch: AtomicU64,
    root: PathBuf,
    room: String,
    host: [u8; 32],
    state: Mutex<State>,
    terminal: Arc<AtomicBool>,
    recovering: AtomicBool,
}
#[derive(Clone)]
pub(crate) struct CommittedAuthority(Arc<Shared>);
pub(crate) enum Observation<'a> {
    Current(Current<'a>),
    Updating,
    Denied,
}
pub(crate) struct Current<'a>(MutexGuard<'a, State>, &'a Shared, Held);
impl Current<'_> {
    pub(crate) fn view(&self) -> &CommittedView {
        match &*self.0 {
            State::Current(view) => view,
            _ => unreachable!("current guard only constructed for committed state"),
        }
    }
    pub(crate) fn continuity(&self) -> u64 {
        self.view().continuity()
    }
    pub(crate) fn peer(&self) -> &StatePeer {
        self.view().peer()
    }
    #[cfg(feature = "media")]
    pub(crate) fn media_policy(&self) -> &crate::room_media_policy::RoomMediaPolicy {
        &self.view().media_policy
    }
    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.view().generation()
    }
}
impl Drop for Current<'_> {
    fn drop(&mut self) {
        self.2.finish("read");
        if std::thread::panicking() {
            if let Some(time) = &self.1.policy_time {
                time.deny(&self.1.terminal);
            } else {
                self.1.terminal.store(true, Ordering::Release);
            }
        }
    }
}
pub(crate) struct OwnerUpdate<'a> {
    held: Held,
    authority: &'a CommittedAuthority,
    state: MutexGuard<'a, State>,
    previous: Option<CommittedView>,
    generation: u64,
    continuity: u64,
    recovering: bool,
}
impl CommittedAuthority {
    pub(crate) fn new(owner: &SessionOwner) -> Self {
        Self::with_policy_clock(
            owner,
            qos_action_broker::peer_actions::OwnerPolicyClock::new(),
        )
    }
    #[cfg(test)]
    pub(crate) fn new_with_test_clock(
        owner: &SessionOwner,
        clock: &qos_action_broker::peer_actions::OwnerPolicyClock,
    ) -> Self {
        Self::with_policy_clock(owner, Ok(clock.clone()))
    }
    fn with_policy_clock(
        owner: &SessionOwner,
        clock: Result<
            qos_action_broker::peer_actions::OwnerPolicyClock,
            qos_action_broker::peer_actions::PolicyClockError,
        >,
    ) -> Self {
        let policy_time = clock.ok().map(private_policy::OwnerTime::new);
        let time_origin = policy_time.as_ref().and_then(|time| time.origin().ok());
        let policy_origin = Arc::new(AtomicBool::new(false));
        let committed = owner.committed_peer();
        let capture_failed = committed.is_err() || time_origin.is_none();
        let state = match committed {
            Ok(peer) if owner.permits_publication() && !capture_failed => {
                State::Current(CommittedView {
                    private_policy: qos_action_broker::peer_actions::PrivatePolicyBasis::from_peer(
                        &peer,
                    )
                    .ok(),
                    policy_epoch: 0,
                    policy_origin: policy_origin.clone(),
                    time_origin,
                    #[cfg(feature = "media")]
                    media_policy: crate::room_media_policy::RoomMediaPolicy::from_committed(&peer),
                    peer,
                    committed_peer_identity: owner.committed_peer_identity(),
                    _serialized_credit: owner.retained_serialized_credit(),
                    generation: owner.committed_generation(),
                    continuity: 0,
                })
            }
            Ok(_) if owner.durability_healthy() && !capture_failed => State::Incomplete {
                generation: owner.committed_generation(),
                continuity: 1,
            },
            _ => State::Denied,
        };
        policy_origin.store(
            !owner.durability_healthy() || capture_failed,
            Ordering::Release,
        );
        if !owner.permits_publication() || capture_failed {
            if let Some(time) = &policy_time {
                time.invalidate();
            }
        }
        Self(Arc::new(Shared {
            #[cfg(test)]
            after_time_retirement: Mutex::new(None),
            policy_time,
            policy_origin: policy_origin.clone(),
            policy_epoch: AtomicU64::new(0),
            root: owner.root().to_owned(),
            room: owner.peer().room().to_owned(),
            host: owner.peer().authority().device_public_key,
            terminal: policy_origin,
            recovering: AtomicBool::new(!owner.permits_publication() || capture_failed),
            state: Mutex::new(state),
        }))
    }
    pub(crate) fn policy_clock(
        &self,
    ) -> Result<qos_action_broker::peer_actions::OwnerPolicyClock, String> {
        self.0
            .policy_time
            .as_ref()
            .map(|time| time.clock())
            .ok_or_else(|| "owner policy time unavailable".into())
    }
    fn invalidate_policy_time(&self) {
        if let Some(time) = &self.0.policy_time {
            time.invalidate();
        }
    }
    fn deny(&self) {
        // Retire queued time before publishing terminal denial. This short
        // origin lock is released before invalidate() waits for a Current guard.
        if let Some(time) = &self.0.policy_time {
            time.deny(&self.0.terminal);
        } else {
            self.0.terminal.store(true, Ordering::Release);
        }
        #[cfg(test)]
        {
            let hook = self.0.after_time_retirement.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
        }
    }
    fn matches(&self, owner: &SessionOwner) -> bool {
        owner.root() == self.0.root
            && owner.peer().room() == self.0.room
            && owner.peer().authority().device_public_key == self.0.host
    }
    fn denied(&self) -> bool {
        self.0.terminal.load(Ordering::Acquire) || self.0.recovering.load(Ordering::Acquire)
    }
    #[track_caller]
    pub(crate) fn observe(&self) -> Observation<'_> {
        if self.denied() {
            return Observation::Denied;
        }
        match self.0.state.try_lock() {
            Ok(state) => {
                if self.denied() {
                    return Observation::Denied;
                }
                match &*state {
                    State::Current(_) => Observation::Current(Current(state, &self.0, Held::new())),
                    State::Updating => Observation::Updating,
                    State::Incomplete { .. } | State::Denied => Observation::Denied,
                }
            }
            Err(TryLockError::WouldBlock) if !self.denied() => Observation::Updating,
            Err(TryLockError::WouldBlock) => Observation::Denied,
            Err(TryLockError::Poisoned(_)) => {
                self.deny();
                Observation::Denied
            }
        }
    }
    #[track_caller]
    pub(crate) fn read(&self) -> Result<Current<'_>, String> {
        let state = self.0.state.lock().map_err(|_| {
            self.deny();
            "owner authority unavailable"
        })?;
        if !self.denied() && matches!(&*state, State::Current(_)) {
            Ok(Current(state, &self.0, Held::new()))
        } else {
            Err("owner authority unavailable".into())
        }
    }
    #[track_caller]
    pub(crate) fn begin(&self, owner: &SessionOwner) -> Result<OwnerUpdate<'_>> {
        ensure!(self.matches(owner), "committed authority owner mismatch");
        let mut state = self.0.state.lock().map_err(|_| {
            self.deny();
            anyhow::anyhow!("owner authority poisoned")
        })?;
        let held = Held::new();
        ensure!(
            !self.0.terminal.load(Ordering::Acquire),
            "owner authority terminally denied"
        );
        if !owner.durability_healthy() {
            self.deny();
            *state = State::Denied;
            anyhow::bail!("owner durability poisoned");
        }
        let (previous, generation, continuity, recovering) =
            match std::mem::replace(&mut *state, State::Denied) {
                State::Current(view) if view.generation == owner.committed_generation() => {
                    let generation = view.generation;
                    let continuity = view.continuity;
                    (Some(view), generation, continuity, false)
                }
                State::Incomplete {
                    generation,
                    continuity,
                } if generation == owner.committed_generation() => {
                    self.invalidate_policy_time();
                    (None, generation, continuity, true)
                }
                _ => {
                    self.deny();
                    anyhow::bail!("owner authority invalid or generation bypassed")
                }
            };
        *state = State::Updating;
        Ok(OwnerUpdate {
            held,
            authority: self,
            state,
            previous,
            generation,
            continuity,
            recovering,
        })
    }
    /// The closure cannot await. Its returned error may mean rejected input,
    /// not failed durability; publish consults the real owner's barrier health.
    #[track_caller]
    pub(crate) fn with_owner<T>(
        &self,
        owner: &mut SessionOwner,
        mutation: impl FnOnce(&mut SessionOwner) -> T,
    ) -> Result<T> {
        let update = self.begin(owner)?;
        let result = mutation(owner);
        update.publish(owner)?;
        Ok(result)
    }
    /// Borrow the retained committed view under the mutation guard. The
    /// synchronous callback must not re-lock this authority; its result is
    /// exposed only after the owner's actual barrier health is published.
    #[track_caller]
    pub(crate) fn with_owner_view<T>(
        &self,
        owner: &mut SessionOwner,
        mutation: impl FnOnce(&CommittedView, &mut SessionOwner) -> T,
    ) -> Result<T> {
        let update = self.begin(owner)?;
        let Some(view) = update.previous.as_ref() else {
            // Recovery turns have no current witness. Publish their existing
            // Incomplete state so rejection does not abandon/poison the guard.
            update.publish(owner)?;
            anyhow::bail!("committed owner view unavailable during recovery");
        };
        let result = mutation(view, owner);
        update.publish(owner)?;
        Ok(result)
    }
    pub(crate) fn invalidate(&self) {
        self.deny();
        if let Ok(mut state) = self.0.state.lock() {
            *state = State::Denied;
        }
    }
}
impl OwnerUpdate<'_> {
    pub(crate) fn publish(mut self, owner: &SessionOwner) -> Result<()> {
        ensure!(
            self.authority.matches(owner),
            "committed authority owner mismatch"
        );
        ensure!(
            !self.authority.0.terminal.load(Ordering::Acquire),
            "owner authority terminally denied"
        );
        ensure!(owner.durability_healthy(), "owner durability unavailable");
        ensure!(
            owner.committed_generation() >= self.generation,
            "owner generation regressed"
        );
        if owner.peer().incomplete_reason().is_some() {
            let continuity = if self.recovering {
                self.continuity
            } else {
                self.continuity
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("authority continuity exhausted"))?
            };
            self.authority.invalidate_policy_time();
            self.authority.0.recovering.store(true, Ordering::Release);
            *self.state = State::Incomplete {
                generation: owner.committed_generation(),
                continuity,
            };
            return Ok(());
        }
        if self.recovering && owner.committed_generation() == self.generation {
            // A live-only prerequisite or a flush is not a new durable view.
            *self.state = State::Incomplete {
                generation: self.generation,
                continuity: self.continuity,
            };
            return Ok(());
        }
        let time_origin = self
            .authority
            .0
            .policy_time
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("owner policy time unavailable"))?;
        let time_origin = if self.recovering {
            time_origin.recover(&self.authority.0.terminal)?
        } else {
            time_origin.origin()?
        };
        // Deferred/rejected input can change volatile queues without a durable
        // commit. Retain the exact previously committed peer in that case.
        let view = match self.previous.take() {
            Some(mut previous) if owner.committed_generation() == previous.generation => {
                // Attachment is runtime custody, not a new authoritative generation.
                previous._serialized_credit = owner.retained_serialized_credit();
                previous
            }
            previous => {
                let peer = owner.committed_peer()?;
                let private_policy =
                    qos_action_broker::peer_actions::PrivatePolicyBasis::from_peer(&peer).ok();
                let unchanged = previous.as_ref().is_some_and(|prior| {
                    private_policy.is_some() && prior.private_policy == private_policy
                });
                let policy_epoch = if unchanged {
                    self.authority.0.policy_epoch.load(Ordering::Acquire)
                } else {
                    self.authority
                        .0
                        .policy_epoch
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                            epoch.checked_add(1)
                        })
                        .map_err(|_| anyhow::anyhow!("private policy lineage exhausted"))?
                        + 1
                };
                CommittedView {
                    private_policy,
                    policy_epoch,
                    policy_origin: self.authority.0.policy_origin.clone(),
                    time_origin: Some(time_origin),
                    #[cfg(feature = "media")]
                    media_policy: crate::room_media_policy::RoomMediaPolicy::from_committed(&peer),
                    peer,
                    committed_peer_identity: owner.committed_peer_identity(),
                    _serialized_credit: owner.retained_serialized_credit(),
                    generation: owner.committed_generation(),
                    continuity: self.continuity,
                }
            }
        };
        *self.state = State::Current(view);
        self.authority.0.recovering.store(false, Ordering::Release);
        Ok(())
    }
}
impl Drop for OwnerUpdate<'_> {
    fn drop(&mut self) {
        self.held.finish("update");
        if matches!(&*self.state, State::Updating) {
            self.authority.deny();
            *self.state = State::Denied;
        }
    }
}
#[cfg(test)]
mod tests;

/// Authority-transition fixture only; this denies a real committed view and
/// does not simulate delivery of any missing MLS/decryption prerequisite.
#[cfg(test)]
pub(crate) fn enter_test_recovery(authority: &CommittedAuthority) {
    let mut state = authority.0.state.lock().unwrap();
    let State::Current(view) = &*state else {
        panic!("test recovery requires an actual current owner");
    };
    let generation = view.generation;
    let continuity = view.continuity.checked_add(1).unwrap();
    authority.invalidate_policy_time();
    authority.0.recovering.store(true, Ordering::Release);
    *state = State::Incomplete {
        generation,
        continuity,
    };
}

mod private_policy;
pub(crate) use private_policy::NativeDisclosureFence;
