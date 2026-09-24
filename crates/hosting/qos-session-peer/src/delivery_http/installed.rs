//! Private data registry transition and eventual installed owner scheduler.
//! No constructor in this module currently qualifies or advertises a data route.
use super::*;
use crate::{host_bytes::HostBytes, owner::resources::OwnerDataOperation};
use tilo_relay::delivery_v3::capabilities;

/// Scheduling output, not current authority or a durable state admission.
pub(super) type DataReady = super::carrier::CarrierReady;

impl HttpSessions {
    pub fn reserve_private_slot(&self, id: [u8; 16]) -> Result<tokio::sync::OwnedSemaphorePermit> {
        let slots = self
            .live
            .get(&id)
            .and_then(|live| live.private_slots.as_ref())
            .context("private session unavailable")?;
        slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow::anyhow!("private session capacity"))
    }
    /// Internal policy constructor. The only current production caller passes0.
    /// Future positive installation must consume the genuine bounded-owner witness.
    #[cfg(test)]
    pub(super) fn new_for_policy(
        owner: &SessionOwner,
        current: &CommittedAuthority,
        now: u64,
        capabilities: u64,
    ) -> Result<Self> {
        ensure!(
            capabilities::validate(capabilities),
            "unknown data capability"
        );
        let guard = current.read().map_err(anyhow::Error::msg)?;
        let descriptor = owner.verified_delivery_transport(guard.view(), at(now), capabilities)?;
        let mut registry = Self::new_unqualified(&descriptor, capabilities, now)?;
        registry.continuity = guard.continuity();
        registry.native_time = Some(guard.view().policy_time().map_err(anyhow::Error::msg)?);
        Ok(registry)
    }

    pub(super) fn new_for_policy_current(
        owner: &SessionOwner,
        current: &CommittedAuthority,
        capabilities: u64,
    ) -> Result<Self> {
        ensure!(
            capabilities::validate(capabilities),
            "unknown data capability"
        );
        let guard = current.read().map_err(anyhow::Error::msg)?;
        let time = guard.view().policy_time().map_err(anyhow::Error::msg)?;
        let now = guard
            .view()
            .check_policy_time(&time)
            .map_err(anyhow::Error::msg)?;
        let descriptor = owner.verified_delivery_transport(guard.view(), at(now), capabilities)?;
        let mut registry = Self::new_unqualified(&descriptor, capabilities, now)?;
        registry.continuity = guard.continuity();
        registry.native_time = Some(time);
        Ok(registry)
    }

    // Provisional pre-negotiation check only. Final publication counts the
    // exact entries it will retire using the verified negotiated capabilities.
    pub(super) fn has_capacity(&self, identity: Identity) -> bool {
        let replacing_data = identity.requested & self.capabilities != 0;
        // Sessions retained from superseded descriptors are bounded by their
        // own generation's admission and do not consume current capacity.
        let retained = self.live.values().filter(|live| {
            live.identity.superseded.is_none()
                && !(replacing_data
                    && live.identity.device == identity.device
                    && live.capabilities != 0)
        });
        retained.clone().count() < LIVE_LIMIT
            && retained
                .filter(|live| live.identity.device == identity.device)
                .count()
                < LIVE_DEVICE
    }
    pub(super) fn publish_ready_clock(
        &mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        verified: auth::VerifiedSession,
        identity: Identity,
        clock: &revalidation::Clock,
        replace_data: bool,
    ) -> Result<DataReady> {
        ensure!(self.has_capacity(identity), "live capacity");
        let guard = current.read().map_err(anyhow::Error::msg)?;
        let now = clock.sample(guard.view())?;
        self.advance(now)?;
        let context = self.context_clock(owner, guard.view(), identity, clock)?;
        ensure!(
            replace_data == (verified.capabilities() != 0),
            "session purpose mismatch"
        );
        self.publish_in(
            super::carrier::CarrierFinish { verified, identity },
            &context.verification(),
        )
    }

    pub(super) fn finish_data(
        &mut self,
        owner: &mut SessionOwner,
        current: &CommittedAuthority,
        budget: &HostBytes,
        body: &[u8],
    ) -> Result<DataReady> {
        let clock = self.native_clock()?;
        self.finish_data_using(owner, current, budget, body, || clock.clone())
    }
    #[cfg(test)]
    pub(super) fn finish_data_at(
        &mut self,
        owner: &mut SessionOwner,
        current: &CommittedAuthority,
        budget: &HostBytes,
        body: &[u8],
        now: u64,
    ) -> Result<DataReady> {
        self.finish_data_clock(owner, current, budget, body, || now)
    }
    #[cfg(test)]
    fn finish_data_clock(
        &mut self,
        owner: &mut SessionOwner,
        current: &CommittedAuthority,
        budget: &HostBytes,
        body: &[u8],
        mut now: impl FnMut() -> u64,
    ) -> Result<DataReady> {
        self.finish_data_using(owner, current, budget, body, || {
            revalidation::Clock::Fixed(now())
        })
    }
    fn finish_data_using(
        &mut self,
        owner: &mut SessionOwner,
        current: &CommittedAuthority,
        budget: &HostBytes,
        body: &[u8],
        mut clock: impl FnMut() -> revalidation::Clock,
    ) -> Result<DataReady> {
        // Real reservation precedes current context/certificate/proof allocations.
        let mut turn = owner.prepare_data_turn(budget, OwnerDataOperation::RegisterConsumer)?;
        let (verified, identity) = self.verify_finish_clock(owner, current, body, &clock())?;
        ensure!(
            verified.capabilities() != 0,
            "media session uses media finish"
        );
        current.with_owner_view(owner, |view, owner| -> Result<()> {
            let clock = clock();
            let time = clock.sample(view)?;
            self.advance(time)?;
            let context = self.context_clock(owner, view, identity, &clock)?;
            verified.revalidate(&context.verification())?;
            let source = owner.data_source(&mut turn, view, &verified, context.now)?;
            owner.data_register_consumer(&mut turn, &source)?;
            Ok(())
        })??;
        // Registration can commit a new generation. Never publish Ready using its
        // pre-registration context/source; even elapsed credential time is checked.
        self.publish_ready_clock(owner, current, verified, identity, &clock(), true)
    }
}
#[cfg(test)]
mod tests;
