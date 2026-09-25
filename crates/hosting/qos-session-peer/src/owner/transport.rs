//! Durable delivery transport bootstrap, owned by the existing SessionOwner.
use super::*;
use crate::committed_authority::CommittedView;
use qos_peer_state::join::TransportHint;
use tilo_relay::delivery_v3::descriptor::{
    self, DescriptorAuthority, DescriptorSettings, GenerationExpectation, VerifiedDescriptor,
};

impl SessionOwner {
    /// Sign an inert invitation locator only from this owner's current installed ingress.
    #[cfg(feature = "media")]
    pub(crate) fn sign_delivery_origin_hint(
        &self,
        installed: &crate::media_ingress::InstalledIngress,
    ) -> Result<TransportHint> {
        let origin = installed.origin_for_discovery(self)?;
        Ok(
            tilo_relay::workspace_join::DeliveryOriginLocatorV1 { version: 1, origin }
                .transport_hint(self.host_certificate(), &self.identity.host)?,
        )
    }

    /// Called on the serialized owner before readiness, or under its publication
    /// update guard. Host admission/delegation completes its real barrier first.
    pub(crate) fn install_delivery_transport(
        &mut self,
        origin: &str,
        capabilities: u64,
        now: SystemTime,
    ) -> Result<TransportHint> {
        self.install_delivery_transport_mask(origin, capabilities, 0, now)
    }

    /// Only the actual running shared ingress can request voice advertisement.
    #[cfg(feature = "media")]
    pub(crate) fn install_running_media_transport(
        &mut self,
        view: &CommittedView,
        installed: &crate::media_ingress::InstalledIngress,
        origin: &str,
        now: SystemTime,
    ) -> Result<TransportHint> {
        self.require_owner_resource_scope()?;
        installed.validate_installation(self, view, origin)?;
        // LIVE_V1 is advertised only while its relay is installed on this endpoint.
        let live = if installed.live_installed() {
            tilo_relay::delivery_v3::live_wire::MEDIA_LIVE_V1
        } else {
            0
        };
        self.install_delivery_transport_mask(
            origin,
            crate::delivery_http::serve::INSTALLED_DATA_CAPABILITIES,
            descriptor::MEDIA_VOICE_V1 | live,
            now,
        )
    }

    fn install_delivery_transport_mask(
        &mut self,
        origin: &str,
        capabilities: u64,
        media_capabilities: u64,
        now: SystemTime,
    ) -> Result<TransportHint> {
        self.require_owner_resource_scope()?;
        self.require_delivery()?;
        anyhow::ensure!(
            self.permits_publication(),
            "native transport authority unavailable"
        );
        if !self.peer.certificates().contains(&self.identity.host_cert) {
            let (candidate, _) = self.prepare_workspace_host_delegation(now)?;
            self.commit_collaboration_candidate(candidate, self.aggregate.collaboration.clone())?;
        }
        let old = match &self.aggregate.publication {
            PublicationStorage::V5 { transport_hint, .. } => transport_hint.clone(),
            _ => bail!("explicit delivery activation required"),
        };
        let previous = old
            .as_ref()
            .map(|hint| descriptor::inspect_persisted_hint(hint, self.host_certificate()))
            .transpose()?;
        let floor = previous.map_or(
            GenerationExpectation::FirstAccepted {
                minimum_generation: 1,
            },
            |m| GenerationExpectation::Current {
                generation: m.generation,
                descriptor_digest: m.descriptor_digest,
            },
        );
        // Renew before expiry: once less than a fifth of the signed lifetime is
        // left, issue the next generation while the current one still verifies,
        // so enrollment/discovery never observes an expired descriptor window.
        let now_ns: u64 = now
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos()
            .try_into()?;
        let renewal_due = previous.is_some_and(|m| {
            let lifetime = m.expires_at_ns.saturating_sub(m.issued_at_ns);
            m.expires_at_ns.saturating_sub(now_ns) < lifetime / 5
        });
        if let Some(hint) = old.as_ref().filter(|_| !renewal_due) {
            let current = self.transport_authority(&self.peer, floor, now, capabilities);
            if let Ok(verified) = descriptor::verify_transport_hint(hint, &current) {
                if verified.origin() == origin
                    && verified.delivery_capabilities() == capabilities
                    && verified.media_capabilities() == media_capabilities
                {
                    return Ok(hint.clone());
                }
            }
        }
        let generation = previous
            .map_or(Some(1), |m| m.generation.checked_add(1))
            .context("descriptor generation exhausted")?;
        let issued_at_ns: u64 = now
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos()
            .try_into()?;
        let mut expires_at_ns = issued_at_ns
            .checked_add(descriptor_lifetime_ns())
            .context("descriptor expiry overflow")?;
        for cert in [self.peer.authority(), self.host_certificate()] {
            if let Some(end) = cert.expires_at_ns {
                expires_at_ns = expires_at_ns.min(end);
            }
        }
        let current = self.transport_authority(&self.peer, floor, now, capabilities);
        let hint = descriptor::issue_transport_hint_with_media(
            &DescriptorSettings {
                origin: origin.into(),
                delivery_capabilities: capabilities,
                issued_at_ns,
                expires_at_ns,
                generation,
            },
            &current,
            &self.identity.host,
            media_capabilities,
        )?;
        let mut next = self.aggregate.clone();
        let PublicationStorage::V5 { transport_hint, .. } = &mut next.publication else {
            bail!("explicit delivery activation required")
        };
        *transport_hint = Some(hint.clone());
        self.commit(next, self.peer.clone())?;
        Ok(hint)
    }
    pub(crate) fn stored_delivery_hint(&self) -> Result<&TransportHint> {
        self.require_delivery()?;
        let PublicationStorage::V5 {
            transport_hint: Some(hint),
            ..
        } = &self.aggregate.publication
        else {
            bail!("delivery transport is not installed")
        };
        Ok(hint)
    }
    pub(crate) fn verified_delivery_transport(
        &self,
        view: &CommittedView,
        now: SystemTime,
        capabilities: u64,
    ) -> Result<VerifiedDescriptor> {
        anyhow::ensure!(
            self.permits_publication()
                && view.generation() == self.committed_generation()
                && view.peer().room() == self.peer.room()
                && view.peer().authority() == self.peer.authority()
                && view.committed_peer_identity() == self.committed_peer_identity(),
            "transport requires this current committed owner view"
        );
        let hint = self.stored_delivery_hint()?;
        let metadata = descriptor::inspect_persisted_hint(hint, self.host_certificate())?;
        let current = self.transport_authority(
            view.peer(),
            GenerationExpectation::Current {
                generation: metadata.generation,
                descriptor_digest: metadata.descriptor_digest,
            },
            now,
            capabilities,
        );
        Ok(descriptor::verify_transport_hint(hint, &current)?)
    }
    pub(crate) fn transport_authority<'a>(
        &'a self,
        peer: &'a StatePeer,
        generation: GenerationExpectation,
        now: SystemTime,
        capabilities: u64,
    ) -> DescriptorAuthority<'a> {
        DescriptorAuthority {
            peer,
            pinned_governor: self.peer.authority(),
            pinned_host: self.host_certificate(),
            workspace: self.workspace_id(),
            generation,
            allowed_delivery_capabilities: capabilities,
            now,
        }
    }
}
/// Signed descriptor lifetime (the delivery contract maximum is 15 minutes).
/// Debug builds accept a shorter lifetime for renewal fault runs; a shorter
/// lifetime only narrows authority. Release builds always use the maximum.
fn descriptor_lifetime_ns() -> u64 {
    const MAXIMUM_NS: u64 = 900_000_000_000;
    #[cfg(debug_assertions)]
    if let Some(seconds) = std::env::var("QOS_TEST_DELIVERY_DESCRIPTOR_LIFETIME_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        return seconds.clamp(60, 900) * 1_000_000_000;
    }
    MAXIMUM_NS
}

#[cfg(test)]
mod tests;
