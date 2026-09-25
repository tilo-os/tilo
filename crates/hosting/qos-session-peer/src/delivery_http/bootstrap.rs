//! Read-only independently governor-signed first-contact bootstrap.
use super::*;
use ed25519_dalek::{Signature, Signer, VerifyingKey};
use tilo_relay::delivery_v3::{
    descriptor,
    media_wire::{BootstrapRequest, BootstrapResponse, join_signature, split_signature},
};
impl HttpSessions {
    pub(crate) fn bootstrap_current(
        &mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        body: &[u8],
    ) -> Result<Vec<u8>> {
        self.bootstrap_clock(owner, current, body, self.native_clock()?)
    }
    #[cfg(test)]
    pub(crate) fn bootstrap(
        &mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        body: &[u8],
        now: u64,
    ) -> Result<Vec<u8>> {
        self.bootstrap_clock(owner, current, body, revalidation::Clock::Fixed(now))
    }
    fn bootstrap_clock(
        &mut self,
        owner: &SessionOwner,
        current: &CommittedAuthority,
        body: &[u8],
        clock: revalidation::Clock,
    ) -> Result<Vec<u8>> {
        self.advance(clock.wall()?)?;
        let request = BootstrapRequest::decode(body)?;
        let guard = current.read().map_err(anyhow::Error::msg)?;
        let now = clock.sample(guard.view())?;
        self.advance(now)?;
        let context = self.context_clock(
            owner,
            guard.view(),
            Identity {
                device: request.device,
                requested: 0,
                mandatory: 0,
                superseded: None,
            },
            &clock,
        )?;
        let now = context.now;
        ensure!(
            request.workspace == owner.workspace_id()
                && request.root == guard.peer().authority().user_public_key
                && request.room == guard.peer().room()
                && request.candidate_digest == self.descriptor,
            "bootstrap scope mismatch"
        );
        auth::Authority::authorize(
            &context.authority,
            &context.binding,
            &context.device,
            context.host,
            now,
        )?;
        VerifyingKey::from_bytes(&context.device.device_public_key)?.verify_strict(
            &request.signing_digest()?,
            &Signature::from_bytes(&join_signature(request.signature)),
        )?;
        ensure!(
            owner.identity.native.verifying_key().to_bytes()
                == guard.peer().authority().device_public_key,
            "governor custody mismatch"
        );
        let hint = owner.stored_delivery_hint()?;
        let metadata = descriptor::inspect_persisted_hint(hint, owner.host_certificate())?;
        let mut response = BootstrapResponse {
            version: 1,
            request,
            host_certificate: postcard::to_stdvec(owner.host_certificate())?,
            hint: postcard::to_stdvec(hint)?,
            generation: metadata.generation,
            descriptor_digest: self.descriptor,
            issued_at_ns: now,
            expires_at_ns: now
                .checked_add(PENDING_NS)
                .context("bootstrap expiry overflow")?
                .min(metadata.expires_at_ns),
            signature: [[0; 32]; 2],
        };
        response.signature = split_signature(
            owner
                .identity
                .native
                .sign(&response.signing_digest()?)
                .to_bytes(),
        );
        Ok(response.encode()?)
    }
}
