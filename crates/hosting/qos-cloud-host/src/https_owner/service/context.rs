//! Signed descriptor custody and fresh carrier contexts under one Store turn.
//! Every context re-verifies the persisted descriptor against Current; the
//! registry pins (boot, digest, capabilities) never stand in for that check.
use super::super::{CloudDeliveryError, CloudStoreTurn, DeliveryOperation};
use anyhow::{Context as _, Result, ensure};
use qos_peer_state::join::TransportHint;
use qos_relay::delivery_v3::{
    auth,
    descriptor::{
        self, DescriptorAuthority, DescriptorSettings, GenerationExpectation, VerifiedDescriptor,
    },
};
use qos_user_identity::{DeviceCert, RevocationList};

/// Signed descriptors live at most 15 minutes; renew with this margin left.
pub(super) const RENEW_MARGIN_NS: u64 = 180_000_000_000;
const LIFETIME_NS: u64 = 900_000_000_000;

/// Test-only renewal trigger keyed by origin, so concurrently running
/// loopback services are unaffected. Production renews only by margin.
#[cfg(test)]
pub(super) mod forced_renewal {
    use std::{collections::BTreeMap, sync::Mutex};
    static FORCED: Mutex<BTreeMap<String, u32>> = Mutex::new(BTreeMap::new());
    pub(in crate::https_owner) fn request(origin: &str, count: u32) {
        *FORCED.lock().unwrap().entry(origin.to_owned()).or_default() += count;
    }
    pub(in crate::https_owner) fn pending(origin: &str) -> bool {
        FORCED.lock().unwrap().get(origin).is_some_and(|n| *n > 0)
    }
    pub(in crate::https_owner) fn consume(origin: &str) {
        if let Some(n) = FORCED.lock().unwrap().get_mut(origin) {
            *n = n.saturating_sub(1);
        }
    }
}
pub(super) fn renewal_forced(_origin: &str) -> bool {
    #[cfg(test)]
    return forced_renewal::pending(_origin);
    #[cfg(not(test))]
    false
}

/// Registry pins captured from the one installed descriptor.
#[derive(Clone, Copy)]
pub(super) struct Pins {
    pub(super) boot: [u8; 16],
    pub(super) descriptor: [u8; 32],
    pub(super) capabilities: u64,
}
/// Fresh borrowed verification inputs; the turn itself is the Authority.
pub(super) struct CloudContext<'a> {
    capabilities: u64,
    binding: auth::Binding,
    device: DeviceCert,
    turn: &'a CloudStoreTurn,
    revocations: RevocationList,
    now: u64,
}
impl CloudContext<'_> {
    pub(super) fn verification(&self) -> auth::VerificationContext<'_> {
        auth::VerificationContext {
            binding: &self.binding,
            capabilities: auth::CapabilityPolicy {
                supported: self.capabilities,
                allowed: self.capabilities,
            },
            credentials: auth::Credentials {
                device: &self.device,
                host: self.turn.peer().authority(),
                device_revocations: &self.revocations,
                host_revocations: &self.revocations,
            },
            authority: self.turn,
            now_ns: self.now,
        }
    }
    pub(super) fn now(&self) -> u64 {
        self.now
    }
}
fn at(now: u64) -> web_time::SystemTime {
    web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now)
}
impl CloudStoreTurn {
    fn stored_hint(&self) -> Option<&TransportHint> {
        self.store.state.delivery.as_ref()?.transport_hint()
    }
    fn descriptor_authority(
        &self,
        generation: GenerationExpectation,
        capabilities: u64,
        now: u64,
    ) -> DescriptorAuthority<'_> {
        // The Cloud host is its own pinned governor and host: the enrolled
        // Store authority. A hint can never appoint either of them.
        DescriptorAuthority {
            peer: self.peer(),
            pinned_governor: self.peer().authority(),
            pinned_host: self.peer().authority(),
            workspace: self.workspace(),
            generation,
            allowed_delivery_capabilities: capabilities,
            now: at(now),
        }
    }
    /// Verify the persisted descriptor against this exact current turn.
    pub(super) fn verified_descriptor(&self, capabilities: u64) -> Result<VerifiedDescriptor> {
        let now = self.fresh_now_ns()?;
        let hint = self
            .stored_hint()
            .context("delivery descriptor not installed")?;
        let metadata = descriptor::inspect_persisted_hint(hint, self.peer().authority())?;
        Ok(descriptor::verify_transport_hint(
            hint,
            &self.descriptor_authority(
                GenerationExpectation::Current {
                    generation: metadata.generation,
                    descriptor_digest: metadata.descriptor_digest,
                },
                capabilities,
                now,
            ),
        )?)
    }
    /// Persist a fresh signed descriptor unless the current one still matches
    /// with at least the renewal margin left. Called before Ready and by the
    /// maintenance tick; the Store commit is the only publication barrier.
    pub(super) fn install_descriptor(
        &mut self,
        origin: &str,
        capabilities: u64,
        media: u64,
    ) -> Result<(VerifiedDescriptor, u64)> {
        self.require_delivery_operation(DeliveryOperation::Maintenance)
            .map_err(anyhow::Error::from)?;
        let now = self.fresh_now_ns()?;
        let host = self.peer().authority().clone();
        let previous = self
            .stored_hint()
            .map(|hint| descriptor::inspect_persisted_hint(hint, &host))
            .transpose()?;
        if let (Some(metadata), Ok(verified)) = (previous, self.verified_descriptor(capabilities)) {
            if verified.origin() == origin
                && verified.delivery_capabilities() == capabilities
                && verified.media_capabilities() == media
                && metadata.expires_at_ns.saturating_sub(now) > RENEW_MARGIN_NS
                && !renewal_forced(origin)
            {
                return Ok((verified, metadata.expires_at_ns));
            }
        }
        let generation = previous
            .map_or(Some(1), |metadata| metadata.generation.checked_add(1))
            .context("descriptor generation exhausted")?;
        let mut expires_at_ns = now.checked_add(LIFETIME_NS).context("expiry overflow")?;
        if let Some(end) = host.expires_at_ns {
            expires_at_ns = expires_at_ns.min(end);
        }
        ensure!(expires_at_ns > now, "host certificate expired");
        let floor = previous.map_or(
            GenerationExpectation::FirstAccepted {
                minimum_generation: 1,
            },
            |metadata| GenerationExpectation::Current {
                generation: metadata.generation,
                descriptor_digest: metadata.descriptor_digest,
            },
        );
        let hint = descriptor::issue_transport_hint_with_media(
            &DescriptorSettings {
                origin: origin.into(),
                delivery_capabilities: capabilities,
                issued_at_ns: now,
                expires_at_ns,
                generation,
            },
            &self.descriptor_authority(floor, capabilities, now),
            &self.store.key(),
            media,
        )?;
        let mut next = self.store.state.clone();
        next.delivery
            .as_mut()
            .context("delivery state missing")?
            .set_transport_hint(hint);
        if let Err(error) = self.store.commit(next) {
            if let Some(failure) = error.downcast_ref::<crate::StoreCommitFailure>() {
                let failure = *failure;
                self.source.retire();
                return Err(CloudDeliveryError::OwnerUncertain(failure).into());
            }
            return Err(error);
        }
        self.refresh_committed()?;
        #[cfg(test)]
        forced_renewal::consume(origin);
        Ok((self.verified_descriptor(capabilities)?, expires_at_ns))
    }
    /// Signed persisted hint bytes for the unauthenticated descriptor route.
    pub(super) fn descriptor_bytes(&self, capabilities: u64, digest: [u8; 32]) -> Result<Vec<u8>> {
        let verified = self.verified_descriptor(capabilities)?;
        ensure!(
            verified.descriptor_digest() == digest,
            "descriptor replaced"
        );
        let hint = self
            .stored_hint()
            .context("delivery descriptor not installed")?;
        let mut bounded = [0; descriptor::MAX_HINT_BYTES];
        Ok(postcard::to_slice(hint, &mut bounded)?.to_vec())
    }
    /// A fresh context for one enrolled device, bound to this registry's pins.
    pub(super) fn carrier_context(
        &self,
        pins: &Pins,
        device: [u8; 32],
        requested: u64,
        mandatory: u64,
    ) -> Result<CloudContext<'_>> {
        let key = CacheKey {
            boot: pins.boot,
            descriptor: pins.descriptor,
            capabilities: pins.capabilities,
            generation: self.generation(),
            device,
            requested,
            mandatory,
        };
        let now = self.fresh_now_ns()?;
        if let Some(hit) = cached_context(&key, self.peer(), now) {
            return Ok(CloudContext {
                // Transcript extensions (multi-group replay) ride beside the
                // descriptor bits; the descriptor itself never carries them.
                capabilities: qos_relay::delivery_v3::capabilities::handshake_policy(
                    pins.capabilities,
                ),
                binding: hit.binding,
                device: hit.device,
                turn: self,
                revocations: hit.revocations,
                now,
            });
        }
        let verified = self.verified_descriptor(pins.capabilities)?;
        ensure!(
            verified.descriptor_digest() == pins.descriptor,
            "descriptor replaced"
        );
        let now = self.fresh_now_ns()?;
        let device = self
            .peer()
            .certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == device)
            .context("device not enrolled")?;
        let binding = verified.server_binding(
            &self.descriptor_authority(
                GenerationExpectation::Current {
                    generation: verified.generation(),
                    descriptor_digest: pins.descriptor,
                },
                pins.capabilities,
                now,
            ),
            &device,
            auth::Carrier::Https,
            requested,
            mandatory,
            pins.boot,
        )?;
        let mut revocations = RevocationList::new();
        for entry in self.peer().revocations() {
            revocations.insert(entry)?;
        }
        let hint = self
            .stored_hint()
            .context("delivery descriptor not installed")?;
        let expires =
            descriptor::inspect_persisted_hint(hint, self.peer().authority())?.expires_at_ns;
        let until = [
            Some(expires),
            self.peer().authority().expires_at_ns,
            device.expires_at_ns,
            now.checked_add(CACHE_TTL_NS),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(now);
        store_context(
            key,
            Cached {
                binding: binding.clone(),
                device: device.clone(),
                revocations: revocations.clone(),
                from: now,
                until,
            },
        );
        Ok(CloudContext {
            capabilities: qos_relay::delivery_v3::capabilities::handshake_policy(pins.capabilities),
            binding,
            device,
            turn: self,
            revocations,
            now,
        })
    }
}

/// F3: memoized carrier-context derivation (descriptor/host verification,
/// signatures, canonical JSON, revocation list). It is a pure function of one
/// committed Store generation plus explicit signed time windows, so it is
/// reused only for the same registry boot, descriptor pin, capabilities,
/// committed generation and requesting identity, and only while `now` is
/// inside the descriptor and certificate windows. The time-dependent roster
/// and role policy are re-evaluated exactly on every use; transcript, proof
/// and authorization checks stay per request.
const CACHE_TTL_NS: u64 = 30_000_000_000;
const CACHE_ENTRIES: usize = 32;
#[derive(Clone, PartialEq, Eq)]
struct CacheKey {
    boot: [u8; 16],
    descriptor: [u8; 32],
    capabilities: u64,
    generation: u64,
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
static CONTEXTS: std::sync::Mutex<Vec<(CacheKey, Cached)>> = std::sync::Mutex::new(Vec::new());
fn cached_context(key: &CacheKey, peer: &qos_peer_state::StatePeer, now: u64) -> Option<Cached> {
    let cached = CONTEXTS
        .lock()
        .ok()?
        .iter()
        .find(|(k, v)| k == key && v.from <= now && now < v.until)
        .map(|(_, v)| v.clone())?;
    let access = peer.governed_access(at(now)).ok()?;
    for device in [key.device, peer.authority().device_public_key] {
        if !access.member_devices.contains(&device) || peer.permissions(device, at(now)).is_err() {
            return None;
        }
    }
    Some(cached)
}
fn store_context(key: CacheKey, value: Cached) {
    let Ok(mut entries) = CONTEXTS.lock() else {
        return;
    };
    entries.retain(|(k, _)| !(k.boot == key.boot && (k.generation != key.generation || k == &key)));
    if entries.len() >= CACHE_ENTRIES {
        entries.remove(0);
    }
    entries.push((key, value));
}
