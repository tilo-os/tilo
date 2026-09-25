//! One owner-scheduled registry for delivery HTTPS authentication and replay admission.
//! No wire metadata constructs authority. Every operation reads the actual committed
//! native owner and derives the binding again from its persisted verified descriptor.
use crate::{
    SessionOwner,
    committed_authority::{CommittedAuthority, CommittedView},
    owner::delivery::NativeDeliveryAuthority,
};
use anyhow::{Context, Result, ensure};
use rand::RngCore;
use std::collections::BTreeMap;
use tilo_relay::delivery_v3::{
    auth,
    descriptor::GenerationExpectation,
    http_proof::{self, HttpSessionAdmission, Request, RequestProof, VerifiedRequest},
};
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::{Duration, SystemTime};
const PENDING_NS: u64 = 30_000_000_000;
const PENDING_LIMIT: usize = 64;
const PENDING_DEVICE: usize = 4;
const LIVE_LIMIT: usize = 16;
const LIVE_DEVICE: usize = 2;
/// Superseded descriptor generations whose live sessions stay admitted after
/// a renewal. Sessions last at most 15 minutes and renewal runs when a fifth
/// of the 15-minute descriptor lifetime is left, so two cover every session.
const RETAINED_GENERATIONS: usize = 2;
#[derive(Clone, Copy)]
struct Identity {
    device: [u8; 32],
    requested: u64,
    mandatory: u64,
    /// The descriptor issuance a session was authenticated under when that
    /// descriptor has since been renewed; `None` for the current descriptor.
    superseded: Option<Superseded>,
}
/// Descriptor issuance identity of a renewed descriptor. A renewal changes
/// only these binding fields (origin, capabilities, host are fixed by the
/// installation), so a retained session's binding is the current binding with
/// exactly these substituted.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Superseded {
    boot: [u8; 16],
    descriptor: [u8; 32],
    generation: u64,
}
struct Pending {
    identity: Identity,
    challenge: auth::Challenge,
    expires: u64,
}
struct Live {
    private_slots: Option<std::sync::Arc<tokio::sync::Semaphore>>,
    entry: u64,
    capabilities: u64,
    identity: Identity,
    admission: HttpSessionAdmission,
    expires: u64,
}
struct CurrentContext<'a> {
    capabilities: u64,
    binding: auth::Binding,
    device: DeviceCert,
    host: &'a DeviceCert,
    revocations: RevocationList,
    authority: NativeDeliveryAuthority<'a>,
    now: u64,
}
impl CurrentContext<'_> {
    fn verification(&self) -> auth::VerificationContext<'_> {
        auth::VerificationContext {
            binding: &self.binding,
            capabilities: auth::CapabilityPolicy {
                supported: self.capabilities,
                allowed: self.capabilities,
            },
            credentials: auth::Credentials {
                device: &self.device,
                host: self.host,
                device_revocations: &self.revocations,
                host_revocations: &self.revocations,
            },
            authority: &self.authority,
            now_ns: self.now,
        }
    }
}
pub struct HttpSessions {
    native_time: Option<qos_action_broker::peer_actions::PolicyTime>,
    capabilities: u64,
    boot: [u8; 16],
    descriptor: [u8; 32],
    descriptor_generation: u64,
    continuity: u64,
    high_water: u64,
    next_entry: u64,
    pending: BTreeMap<[u8; 16], Pending>,
    live: BTreeMap<[u8; 16], Live>,
    context_cache: revalidation::ContextCache,
}
impl HttpSessions {
    pub(crate) fn descriptor_digest(&self) -> [u8; 32] {
        self.descriptor
    }
    pub(crate) fn live_sessions(&self) -> usize {
        self.live.len()
    }
    #[cfg(test)]
    pub(crate) fn has_session(&self, id: [u8; 16]) -> bool {
        self.live.contains_key(&id)
    }
    /// Install `self` (the registry for a renewed descriptor) in place of
    /// `previous`, keeping `previous`'s live sessions valid: they keep their
    /// original binding until they expire, are replaced by the same device's
    /// new data session, or fall more than two renewals behind. Pending
    /// challenges are dropped (the client simply retries the handshake).
    /// Anything other than a same-installation renewal replaces the registry.
    pub(crate) fn inherit_renewed(&mut self, previous: HttpSessions) {
        let same_installation = previous.capabilities == self.capabilities
            && previous.continuity == self.continuity
            && previous.descriptor_generation < self.descriptor_generation
            && match (&previous.native_time, &self.native_time) {
                (Some(old), Some(new)) => old.same_origin(new),
                (None, None) => true,
                _ => false,
            };
        if !same_installation {
            return;
        }
        let renewed = Superseded {
            boot: previous.boot,
            descriptor: previous.descriptor,
            generation: previous.descriptor_generation,
        };
        let floor = previous
            .descriptor_generation
            .saturating_sub(RETAINED_GENERATIONS as u64 - 1);
        self.high_water = self.high_water.max(previous.high_water);
        self.next_entry = self.next_entry.max(previous.next_entry);
        for (id, mut live) in previous.live {
            let pin = *live.identity.superseded.get_or_insert(renewed);
            if pin.generation >= floor && live.expires > self.high_water {
                self.live.insert(id, live);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn new(
        owner: &SessionOwner,
        current: &CommittedAuthority,
        now: u64,
    ) -> Result<Self> {
        // The registry negotiates the intersection with the owner's actual
        // descriptor; capability-zero fixture descriptors still enable none.
        Self::new_for_policy(owner, current, now, serve::INSTALLED_DATA_CAPABILITIES)
    }
    fn advance(&mut self, now: u64) -> Result<()> {
        ensure!(now >= self.high_water, "clock rollback");
        self.high_water = now;
        self.pending.retain(|_, v| v.expires > now);
        self.live.retain(|_, v| v.expires > now);
        Ok(())
    }
    fn context_clock<'a>(
        &mut self,
        owner: &'a SessionOwner,
        view: &'a CommittedView,
        identity: Identity,
        clock: &revalidation::Clock,
    ) -> Result<CurrentContext<'a>> {
        revalidation::ContextPins::capture(self, identity).context(
            owner,
            view,
            clock,
            &mut self.high_water,
        )
    }
    #[cfg(test)]
    fn context<'a>(
        &mut self,
        owner: &'a SessionOwner,
        view: &'a CommittedView,
        identity: Identity,
        now: u64,
    ) -> Result<CurrentContext<'a>> {
        self.context_clock(owner, view, identity, &revalidation::Clock::Fixed(now))
    }
    fn native_clock(&self) -> Result<revalidation::Clock> {
        Ok(revalidation::Clock::Native(
            self.native_time
                .clone()
                .context("native owner clock not registered")?,
        ))
    }
    pub(crate) fn new_current(owner: &SessionOwner, current: &CommittedAuthority) -> Result<Self> {
        Self::new_for_policy_current(owner, current, serve::INSTALLED_DATA_CAPABILITIES)
    }
    pub(crate) fn challenge_current(
        &mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        body: &[u8],
    ) -> Result<Vec<u8>> {
        self.challenge_clock(owner, current, body, self.native_clock()?)
    }
    #[cfg(test)]
    pub(crate) fn challenge(
        &mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        body: &[u8],
        now: u64,
    ) -> Result<Vec<u8>> {
        self.challenge_clock(owner, current, body, revalidation::Clock::Fixed(now))
    }
    fn challenge_clock(
        &mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        body: &[u8],
        clock: revalidation::Clock,
    ) -> Result<Vec<u8>> {
        self.advance(clock.wall()?)?;
        let identity = Self::challenge_intent(body)?.0;
        let guard = current.read().map_err(anyhow::Error::msg)?;
        let now = clock.sample(guard.view())?;
        self.advance(now)?;
        let context = self.context_clock(owner, guard.view(), identity, &clock)?;
        self.challenge_in(body, &context.verification(), &owner.identity.host)
    }
    pub(crate) fn finish_current(
        &mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        body: &[u8],
    ) -> Result<Vec<u8>> {
        self.finish_clock(owner, current, body, self.native_clock()?)
    }
    #[cfg(test)]
    pub(crate) fn finish(
        &mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        body: &[u8],
        now: u64,
    ) -> Result<Vec<u8>> {
        self.finish_clock(owner, current, body, revalidation::Clock::Fixed(now))
    }
    fn finish_clock(
        &mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        body: &[u8],
        clock: revalidation::Clock,
    ) -> Result<Vec<u8>> {
        let (verified, identity) = self.verify_finish_clock(owner, current, body, &clock)?;
        ensure!(
            verified.capabilities() == 0,
            "data consumer admission required"
        );
        Ok(self
            .publish_ready_clock(owner, current, verified, identity, &clock, false)?
            .bytes)
    }
    fn verify_finish_clock(
        &mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        body: &[u8],
        clock: &revalidation::Clock,
    ) -> Result<(auth::VerifiedSession, Identity)> {
        self.advance(clock.wall()?)?;
        let proof = auth::ClientProof::decode(body)?;
        let identity = self.finish_intent(body)?.0;
        let result = (|| {
            let guard = current.read().map_err(anyhow::Error::msg)?;
            let now = clock.sample(guard.view())?;
            self.advance(now)?;
            let context = self.context_clock(owner, guard.view(), identity, &clock)?;
            self.finish_in(body, &context.verification())
        })();
        // Failure still consumes the challenge even when the concrete owner
        // fails before entering owner-neutral mutual authentication.
        if result.is_err() {
            self.pending.remove(&proof.session_id);
        }
        let finish = result?;
        Ok((finish.verified, finish.identity))
    }
    pub(crate) fn admit_current<'a>(
        &'a mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        proof: &RequestProof,
        request: &'a Request<'a>,
    ) -> Result<VerifiedRequest<'a>> {
        self.admit_clock(owner, current, proof, request, self.native_clock()?)
    }
    #[cfg(test)]
    pub(crate) fn admit<'a>(
        &'a mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        proof: &RequestProof,
        request: &'a Request<'a>,
        now: u64,
    ) -> Result<VerifiedRequest<'a>> {
        self.admit_clock(
            owner,
            current,
            proof,
            request,
            revalidation::Clock::Fixed(now),
        )
    }
    fn admit_clock<'a>(
        &'a mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        proof: &RequestProof,
        request: &'a Request<'a>,
        clock: revalidation::Clock,
    ) -> Result<VerifiedRequest<'a>> {
        self.advance(clock.wall()?)?;
        let id = proof.session_id();
        let identity = self.live.get(&id).context("unknown session")?.identity;
        let guard = current.read().map_err(anyhow::Error::msg)?;
        let now = clock.sample(guard.view())?;
        self.advance(now)?;
        let context = self.context_clock(owner, guard.view(), identity, &clock)?;
        self.admit_in(proof, request, &context.verification())
    }
}
fn at(ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_nanos(ns)
}
#[cfg(test)]
pub(crate) mod tests;

mod bootstrap;

mod data;
mod poll;
pub(crate) mod serve;

pub(crate) mod revalidation;
#[cfg(test)]
use revalidation::TestClock;

mod installed;
pub(crate) mod upload;

pub(crate) mod snapshot;

pub(crate) mod carrier;
