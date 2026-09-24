//! Local v2 handoff consumes the actual committed enrollment receipt and pin.
use super::payload::Payload;
use super::reply::{Pinned, current_device};
use super::*;
use crate::enrollment::LocalOnboardingProof;
use crate::owner::resources::OwnedPublicationPin;
use ed25519_dalek::Signer;
use tilo_relay::delivery_v3::{
    media_wire::split_signature,
    onboarding_wire::{HandoffManifest, HandoffPurpose},
};

impl OnboardingRegistry {
    /// Call inside the same real OwnerScope, immediately after enrollment.
    pub(crate) fn pin_local(&self, owner: &SessionOwner) -> Result<OwnedPublicationPin> {
        owner
            .pin_publication(&self.budget)
            .map_err(anyhow::Error::new)
    }
    /// The host preserves the original command lease and cancellation deadline
    /// through publication/reconciliation, then passes its exact receipt here.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit protocol inputs stay separate parameters"
    )]
    pub(crate) fn local_bootstrap(
        &mut self,
        owner: &SessionOwner,
        view: &CommittedView,
        certificate: &DeviceCert,
        proof: LocalOnboardingProof,
        pin: OwnedPublicationPin,
        metadata: serde_json::Value,
        now: u64,
    ) -> Result<Vec<u8>> {
        ensure!(now >= self.high_water, "onboarding clock regressed");
        self.high_water = now;
        let challenge = proof.challenge();
        ensure!(
            now < challenge.expires_at_ns
                && challenge.device_public_key == certificate.device_public_key
                && challenge.root_public_key == owner.peer().authority().user_public_key
                && challenge.room == owner.peer().room()
                && challenge.workspace_id == owner.workspace_id()
                && pin.generation() == owner.committed_generation(),
            "local handoff receipt scope changed"
        );
        ensure!(
            owner
                .issuances()
                .iter()
                .any(|issued| issued.request_id == challenge.request_id
                    && issued.certificate == *certificate),
            "local handoff lacks exact committed issuance"
        );
        current_device(owner, certificate, now, true)?;
        let bootstrap = metadata
            .get("bootstrap")
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| anyhow::anyhow!("local bootstrap metadata missing"))?;
        ensure!(
            [
                "governance_messages",
                "governance_retention",
                "governance_generation",
                "governance_handoff"
            ]
            .iter()
            .all(|field| !bootstrap.contains_key(*field)),
            "local governance already supplied"
        );
        let descriptor = owner.verified_delivery_transport(view, at(now), PRODUCT_DATA_V1)?;
        if let Some(bytes) = crate::enrollment::encode_inline_bootstrap(&metadata, owner)
            .map_err(|error| anyhow::anyhow!("{}", error.message))?
        {
            return Ok(bytes);
        }
        self.pins.retain(|pin| pin.expires > now);
        if self.pins.len() == 8 {
            return Err(unavailable());
        }
        let metadata_credit = self
            .budget
            .try_acquire_bulk(24 * 1024)
            .ok_or_else(unavailable)?;
        let payload = Payload::Governance(pin);
        let length = payload.len()?;
        let (content_digest, segments) = payload.hashes()?;
        let expires = now
            .checked_add(30_000_000_000)
            .ok_or_else(|| anyhow::anyhow!("handoff expiry overflow"))?
            .min(challenge.expires_at_ns);
        let mut manifest = HandoffManifest {
            version: 1,
            purpose: HandoffPurpose::LocalGovernance,
            scope: OnboardingScope {
                origin: descriptor.origin(),
                host: owner.host_certificate().device_public_key,
                workspace: owner.workspace_id(),
                root: owner.peer().authority().user_public_key,
                room: owner.peer().room().parse()?,
            },
            device: certificate.device_public_key,
            request_digest: proof.request_digest(),
            request_nonce: challenge.nonce,
            owner_generation: owner.committed_generation(),
            server_boot_id: self.boot,
            issued_at_ns: now,
            expires_at_ns: expires,
            total_bytes: length as u64,
            content_digest,
            segments,
            signature: [[0; 32]; 2],
        };
        manifest.signature = split_signature(
            owner
                .identity
                .host
                .sign(&manifest.signing_digest()?)
                .to_bytes(),
        );
        let encoded = manifest.encode()?;
        let output = crate::enrollment::encode_handoff_bootstrap(
            &metadata,
            &encoded,
            owner.committed_generation(),
        )
        .map_err(|error| anyhow::anyhow!("{}", error.message))?;
        self.pins.push(Pinned {
            manifest: encoded,
            payload,
            descriptor: descriptor.descriptor_digest(),
            device: certificate.clone(),
            expires,
            next_offset: 0,
            last_range: None,
            _payload_credit: None,
            _metadata_credit: metadata_credit,
        });
        Ok(output)
    }
}
