//! Private admission/continuation custody. This module does not install a route.
use super::*;
use crate::{
    host_bytes::HostBytes,
    https::BufferedRequest,
    owner::resources::{OwnerDataOperation, OwnerResourceTurn},
};
use tokio::{sync::watch, time::Instant};

#[derive(Clone)]
pub(super) struct ContextPins {
    native_time: Option<qos_action_broker::peer_actions::PolicyTime>,
    capabilities: u64,
    /// Binding pins of this session: the current registry's, or the renewed
    /// descriptor issuance it was authenticated under (`identity.superseded`).
    boot: [u8; 16],
    descriptor: [u8; 32],
    /// The registry's current descriptor, always re-derived and verified.
    current: [u8; 32],
    continuity: u64,
    identity: Identity,
    cache: ContextCache,
}
impl ContextPins {
    pub(super) fn capture(registry: &HttpSessions, identity: Identity) -> Self {
        Self {
            native_time: registry.native_time.clone(),
            capabilities: registry.capabilities,
            boot: identity.superseded.map_or(registry.boot, |pin| pin.boot),
            descriptor: identity
                .superseded
                .map_or(registry.descriptor, |pin| pin.descriptor),
            current: registry.descriptor,
            continuity: registry.continuity,
            identity,
            cache: registry.context_cache.clone(),
        }
    }
    pub(super) fn context<'a>(
        &self,
        owner: &'a SessionOwner,
        view: &'a CommittedView,
        clock: &Clock,
        high_water: &mut u64,
    ) -> Result<CurrentContext<'a>> {
        let identity = self.identity;
        let registered = self
            .native_time
            .as_ref()
            .context("native owner clock not registered")?;
        // This final checked sample is the time of every decision made by the
        // context. Never observe a later native value and then reuse preflight time.
        let now = clock.sample(view)?;
        ensure!(now >= *high_water, "clock rollback");
        *high_water = now;
        match clock {
            Clock::Native(time) => {
                ensure!(time.same_origin(registered), "native clock origin changed")
            }
            // Numeric fixture clocks still require the actual owner's origin,
            // but intentionally control their own verification timestamp.
            #[cfg(test)]
            _ => {
                view.check_policy_time(registered)
                    .map_err(anyhow::Error::msg)?;
            }
        }
        ensure!(
            view.continuity() == self.continuity,
            "authority continuity changed"
        );
        let key = CacheKey {
            generation: view.generation(),
            continuity: self.continuity,
            peer: view.committed_peer_identity(),
            capabilities: self.capabilities,
            descriptor: self.descriptor,
            current: self.current,
            boot: self.boot,
            device: identity.device,
            requested: identity.requested,
            mandatory: identity.mandatory,
        };
        if let Some(hit) = self.cache.hit(&key, owner, view, now) {
            return Ok(CurrentContext {
                capabilities: self.capabilities,
                binding: hit.binding,
                device: hit.device,
                host: owner.host_certificate(),
                revocations: hit.revocations,
                authority: NativeDeliveryAuthority {
                    view,
                    workspace: owner.workspace_id(),
                    host: owner.host_certificate(),
                },
                now,
            });
        }
        let descriptor = owner.verified_delivery_transport(view, at(now), self.capabilities)?;
        ensure!(
            descriptor.descriptor_digest() == self.current,
            "descriptor replaced"
        );
        let device = view
            .peer()
            .certificates()
            .iter()
            .find(|v| v.device_public_key == identity.device)
            .cloned()
            .context("device not enrolled")?;
        let descriptor_authority = owner.transport_authority(
            view.peer(),
            GenerationExpectation::Current {
                generation: descriptor.generation(),
                descriptor_digest: self.current,
            },
            at(now),
            self.capabilities,
        );
        let mut binding = descriptor.server_binding(
            &descriptor_authority,
            &device,
            auth::Carrier::Https,
            identity.requested,
            identity.mandatory,
            self.boot,
        )?;
        // A session retained across a renewal is verified against a context
        // freshly derived from the current verified descriptor; only the
        // descriptor issuance identity it was authenticated under differs.
        if let Some(pin) = identity.superseded {
            ensure!(
                pin.generation < binding.descriptor_generation,
                "superseded descriptor is not older"
            );
            binding.descriptor_digest = pin.descriptor;
            binding.descriptor_generation = pin.generation;
        }
        let mut revocations = RevocationList::new();
        for entry in view.peer().revocations() {
            revocations.insert(entry)?;
        }
        // Everything above is a pure function of this committed generation
        // except its time bounds: the signed descriptor window and the
        // governor, host and device certificate windows.
        let expires = tilo_relay::delivery_v3::descriptor::inspect_persisted_hint(
            owner.stored_delivery_hint()?,
            owner.host_certificate(),
        )?
        .expires_at_ns;
        let until = [
            Some(expires),
            view.peer().authority().expires_at_ns,
            owner.host_certificate().expires_at_ns,
            device.expires_at_ns,
            now.checked_add(CACHE_TTL_NS),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(now);
        self.cache.store(
            key,
            Cached {
                binding: binding.clone(),
                device: device.clone(),
                revocations: revocations.clone(),
                from: now,
                until,
            },
        );
        Ok(CurrentContext {
            capabilities: self.capabilities,
            binding,
            device,
            host: owner.host_certificate(),
            revocations,
            authority: NativeDeliveryAuthority {
                view,
                workspace: owner.workspace_id(),
                host: owner.host_certificate(),
            },
            now,
        })
    }
}
/// F3: memoized current-context derivation. The descriptor, host and
/// governor verification (signatures, canonical JSON, governance parse) is a
/// pure function of one committed generation plus explicit time windows, so
/// it is reused only for the exact same committed view identity, continuity,
/// descriptor pin, boot and requesting identity, and only while `now` stays
/// inside every signed window. Time-dependent policy (member roster at `now`,
/// role/permissions at `now`) is re-evaluated exactly on every use, as are the
/// per-request transcript, proof and native authorization checks.
const CACHE_TTL_NS: u64 = 30_000_000_000;
const CACHE_ENTRIES: usize = 32;
#[derive(Clone, PartialEq, Eq)]
pub(super) struct CacheKey {
    generation: u64,
    continuity: u64,
    peer: [u8; 32],
    capabilities: u64,
    descriptor: [u8; 32],
    current: [u8; 32],
    boot: [u8; 16],
    device: [u8; 32],
    requested: u64,
    mandatory: u64,
}
#[derive(Clone)]
struct Cached {
    binding: auth::Binding,
    device: DeviceCert,
    revocations: RevocationList,
    from: u64,
    until: u64,
}
#[derive(Clone, Default)]
pub(crate) struct ContextCache(std::sync::Arc<std::sync::Mutex<Vec<(CacheKey, Cached)>>>);
impl ContextCache {
    fn hit(
        &self,
        key: &CacheKey,
        owner: &SessionOwner,
        view: &CommittedView,
        now: u64,
    ) -> Option<Cached> {
        // The same owner guards as `verified_delivery_transport`: this exact
        // current committed generation, publication still permitted.
        if !owner.permits_publication() || view.generation() != owner.committed_generation() {
            return None;
        }
        let cached = {
            let entries = self.0.lock().ok()?;
            entries
                .iter()
                .find(|(k, v)| k == key && v.from <= now && now < v.until)
                .map(|(_, v)| v.clone())?
        };
        // Exact time-dependent policy for the requesting device and the host.
        let peer = view.peer();
        let access = peer.governed_access(at(now)).ok()?;
        let host = owner.host_certificate().device_public_key;
        for device in [key.device, host] {
            if !access.member_devices.contains(&device)
                || peer.permissions(device, at(now)).is_err()
            {
                return None;
            }
        }
        Some(cached)
    }
    fn store(&self, key: CacheKey, value: Cached) {
        let Ok(mut entries) = self.0.lock() else {
            return;
        };
        entries.retain(|(k, _)| k.generation == key.generation && k != &key);
        if entries.len() >= CACHE_ENTRIES {
            entries.remove(0);
        }
        entries.push((key, value));
    }
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.0.lock().map(|entries| entries.len()).unwrap_or(0)
    }
}
struct ContextSeed {
    pins: ContextPins,
    session: [u8; 16],
    entry: u64,
}
#[derive(Clone)]
pub(super) enum Clock {
    Native(qos_action_broker::peer_actions::PolicyTime),
    #[cfg(test)]
    Fixed(u64),
    #[cfg(test)]
    Test(TestClock),
}
impl Clock {
    pub(super) fn wall(&self) -> Result<u64> {
        match self {
            Self::Native(time) => time.now_ns().map_err(anyhow::Error::msg),
            #[cfg(test)]
            Self::Fixed(now) => Ok(*now),
            #[cfg(test)]
            Self::Test(clock) => Ok(clock.wall.load(std::sync::atomic::Ordering::SeqCst)),
        }
    }
    pub(super) fn sample(&self, view: &CommittedView) -> Result<u64> {
        match self {
            Self::Native(time) => view.check_policy_time(time).map_err(anyhow::Error::msg),
            #[cfg(test)]
            _ => self.wall(),
        }
    }
    fn instant(&self) -> Instant {
        match self {
            Self::Native(_) => Instant::now(),
            #[cfg(test)]
            Self::Fixed(_) => Instant::now(),
            #[cfg(test)]
            Self::Test(clock) => {
                clock.base
                    + std::time::Duration::from_nanos(
                        clock.elapsed.load(std::sync::atomic::Ordering::SeqCst),
                    )
            }
        }
    }
}
/// Borrowed from disjoint registry fields alongside the actual admitted request.
/// No public constructor, detached authority or callback-selected current time.
pub(super) struct OperationRevalidation<'a> {
    operation: OwnerDataOperation,
    high_water: &'a mut u64,
    seed: ContextSeed,
    deadline: Instant,
    cancellation: watch::Receiver<bool>,
    clock: Clock,
}
fn cancelled(cancellation: &watch::Receiver<bool>, deadline: Instant, clock: &Clock) -> bool {
    *cancellation.borrow() || cancellation.has_changed().is_err() || clock.instant() >= deadline
}
impl OperationRevalidation<'_> {
    pub(super) fn check(
        &mut self,
        owner: &SessionOwner,
        view: &CommittedView,
        session: &auth::VerifiedSession,
    ) -> Result<u64> {
        ensure!(
            !cancelled(&self.cancellation, self.deadline, &self.clock),
            "original request cancelled or expired"
        );
        let now = self.clock.sample(view)?;
        ensure!(now >= *self.high_water, "clock rollback");
        // A failed authority check still observes time. It cannot later rewind
        // the registry and resurrect a previously expired proof/session.
        *self.high_water = now;
        ensure!(
            session.session_id() == self.seed.session,
            "operation session replaced"
        );
        let context = self
            .seed
            .pins
            .context(owner, view, &self.clock, self.high_water)?;
        session.revalidate(&context.verification())?;
        Ok(context.now)
    }
    pub(super) fn entry_serial(&self) -> u64 {
        self.seed.entry
    }
}
#[cfg(test)]
#[derive(Clone)]
pub(super) struct TestClock {
    wall: std::sync::Arc<std::sync::atomic::AtomicU64>,
    elapsed: std::sync::Arc<std::sync::atomic::AtomicU64>,
    base: Instant,
}
#[cfg(test)]
impl TestClock {
    pub(super) fn new(now: u64) -> Self {
        Self {
            wall: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(now)),
            elapsed: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            base: Instant::now(),
        }
    }
    pub(super) fn set_wall(&self, now: u64) {
        self.wall.store(now, std::sync::atomic::Ordering::SeqCst);
    }
    pub(super) fn advance_monotonic(&self, elapsed: std::time::Duration) {
        let nanos: u64 = elapsed.as_nanos().try_into().unwrap();
        self.elapsed
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |old| old.checked_add(nanos),
            )
            .unwrap();
    }
}
impl HttpSessions {
    pub(super) fn admit_revalidating<'a>(
        &'a mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        budget: &HostBytes,
        operation: OwnerDataOperation,
        proof: &RequestProof,
        exact: &'a Request<'a>,
        original: &BufferedRequest,
    ) -> Result<(
        VerifiedRequest<'a>,
        OperationRevalidation<'a>,
        OwnerResourceTurn,
    )> {
        self.admit_with_clock(
            owner,
            current,
            budget,
            operation,
            proof,
            exact,
            original,
            self.native_clock()?,
        )
    }
    #[cfg(test)]
    pub(super) fn admit_revalidating_at<'a>(
        &'a mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        budget: &HostBytes,
        operation: OwnerDataOperation,
        proof: &RequestProof,
        exact: &'a Request<'a>,
        original: &BufferedRequest,
        clock: &TestClock,
    ) -> Result<(
        VerifiedRequest<'a>,
        OperationRevalidation<'a>,
        OwnerResourceTurn,
    )> {
        self.admit_with_clock(
            owner,
            current,
            budget,
            operation,
            proof,
            exact,
            original,
            Clock::Test(clock.clone()),
        )
    }
    fn admit_with_clock<'a>(
        &'a mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        budget: &HostBytes,
        operation: OwnerDataOperation,
        proof: &RequestProof,
        exact: &'a Request<'a>,
        original: &BufferedRequest,
        clock: Clock,
    ) -> Result<(
        VerifiedRequest<'a>,
        OperationRevalidation<'a>,
        OwnerResourceTurn,
    )> {
        // This real owner/pool-bound reservation precedes authority metadata,
        // current context, proof encoding and nonce-cache admission allocations.
        let turn = owner.prepare_data_turn(budget, operation)?;
        let now = clock.wall()?;
        self.advance(now)?;
        let cancellation = original.cancellation();
        ensure!(
            !cancelled(&cancellation, original.deadline(), &clock),
            "original request cancelled or expired"
        );
        let original_exact = Request::new(
            original.method().as_str(),
            original.target(),
            original.body(),
        )?;
        ensure!(
            original_exact.method() == exact.method()
                && original_exact.target() == exact.target()
                && original_exact.body() == exact.body(),
            "request bytes differ from original transport"
        );
        let proof_bytes = proof.encode()?;
        ensure!(
            original.proof() == Some(proof_bytes.as_slice()),
            "proof differs from original transport"
        );
        let id = proof.session_id();
        let live = self.live.get(&id).context("unknown session")?;
        let seed = ContextSeed {
            pins: ContextPins::capture(self, live.identity),
            session: id,
            entry: live.entry,
        };
        let guard = current.read().map_err(anyhow::Error::msg)?;
        let now = clock.sample(guard.view())?;
        self.advance(now)?;
        let context = seed
            .pins
            .context(owner, guard.view(), &clock, &mut self.high_water)?;
        let admitted = self
            .live
            .get_mut(&id)
            .context("unknown session")?
            .admission
            .admit(proof, exact, &context.verification())?;
        let check = OperationRevalidation {
            operation,
            high_water: &mut self.high_water,
            seed,
            deadline: original.deadline(),
            cancellation,
            clock,
        };
        Ok((admitted, check, turn))
    }
    pub(super) fn resume_poll<'a>(
        &'a mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        budget: &HostBytes,
        pending: &poll::PendingPoll,
    ) -> Result<(
        &'a auth::VerifiedSession,
        OperationRevalidation<'a>,
        OwnerResourceTurn,
    )> {
        self.resume_poll_with_clock(owner, current, budget, pending, self.native_clock()?)
    }
    #[cfg(test)]
    pub(super) fn resume_poll_at<'a>(
        &'a mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        budget: &HostBytes,
        pending: &poll::PendingPoll,
        clock: &TestClock,
    ) -> Result<(
        &'a auth::VerifiedSession,
        OperationRevalidation<'a>,
        OwnerResourceTurn,
    )> {
        self.resume_poll_with_clock(owner, current, budget, pending, Clock::Test(clock.clone()))
    }
    fn resume_poll_with_clock<'a>(
        &'a mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        budget: &HostBytes,
        pending: &poll::PendingPoll,
        clock: Clock,
    ) -> Result<(
        &'a auth::VerifiedSession,
        OperationRevalidation<'a>,
        OwnerResourceTurn,
    )> {
        let turn = owner.prepare_data_turn(budget, OwnerDataOperation::Replay)?;
        let now = clock.wall()?;
        self.advance(now)?;
        let original = pending.original();
        let cancellation = original.cancellation();
        ensure!(
            !cancelled(&cancellation, original.deadline(), &clock),
            "original poll cancelled or expired"
        );
        let id = pending.session_id();
        let live = self.live.get(&id).context("poll session replaced")?;
        let seed = ContextSeed {
            pins: ContextPins::capture(self, live.identity),
            session: id,
            entry: live.entry,
        };
        let guard = current.read().map_err(anyhow::Error::msg)?;
        let now = clock.sample(guard.view())?;
        self.advance(now)?;
        let context = seed
            .pins
            .context(owner, guard.view(), &clock, &mut self.high_water)?;
        // No proof/nonce readmission. Only the same actual registry-owned opaque
        // session can continue the retained original admission ticket.
        let session = self
            .live
            .get_mut(&id)
            .context("poll session replaced")?
            .admission
            .revalidate(&context.verification())?;
        pending
            .check_session(session, seed.entry, context.now)
            .map_err(|error| anyhow::anyhow!("poll ticket rejected: {error:?}"))?;
        let check = OperationRevalidation {
            operation: OwnerDataOperation::Replay,
            high_water: &mut self.high_water,
            seed,
            deadline: original.deadline(),
            cancellation,
            clock,
        };
        Ok((session, check, turn))
    }
}
#[cfg(test)]
mod tests;

pub(crate) mod continuation;
pub(super) use continuation::PendingOperation;
