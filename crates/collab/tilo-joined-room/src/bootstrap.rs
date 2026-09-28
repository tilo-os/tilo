use crate::{NativeCommittedView, NativeWorkspaceOwner, RoomId, SharedOwner, enrollment, record};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use std::{
    sync::{Arc, Weak},
    time::SystemTime,
};
use tilo_delivery_protocol::delivery_v3::{
    auth::Carrier,
    descriptor::{self, DescriptorAuthority, VerifiedDescriptor},
    media_wire,
};
use tilo_peer_state::join::TransportHint;

/// One actual owner and continuity retain the exact typed request across HTTP.
pub struct NativeBootstrap {
    owner: Weak<SharedOwner>,
    room: RoomId,
    continuity: u64,
    request: media_wire::BootstrapRequest,
    descriptor: VerifiedDescriptor,
    hint: TransportHint,
    hint_bytes: Vec<u8>,
}
impl NativeBootstrap {
    #[must_use]
    pub fn request(&self) -> &media_wire::BootstrapRequest {
        &self.request
    }
    #[must_use]
    pub fn origin(&self) -> &str {
        self.descriptor.origin()
    }
}
impl NativeCommittedView {
    fn descriptor_authority(&self, now: SystemTime) -> Result<DescriptorAuthority<'_>, String> {
        Ok(DescriptorAuthority {
            peer: &self.peer,
            pinned_governor: self.governor(),
            pinned_host: self
                .host()
                .ok_or("no independently pinned native delivery host")?,
            workspace: self.workspace(),
            generation: self.descriptor_expectation()?,
            allowed_delivery_capabilities:
                tilo_delivery_protocol::delivery_v3::capabilities::PRODUCT_DATA_V1,
            now,
        })
    }
}
impl NativeWorkspaceOwner {
    /// Start authenticating a delivery descriptor for `room` with the host.
    ///
    /// # Errors
    /// `key` is not the enrolled device, or the descriptor does not verify
    /// against the pinned governor and host.
    pub fn begin_bootstrap(
        &self,
        room: RoomId,
        candidate: &TransportHint,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NativeBootstrap, String> {
        self.with_current(room, |view| {
            if key.verifying_key().to_bytes() != view.device().device_public_key {
                return Err("native bootstrap key custody mismatch".into());
            }
            let authority = view.descriptor_authority(now)?;
            let descriptor = descriptor::verify_transport_hint(candidate, &authority)
                .map_err(|e| e.to_string())?;
            let locator = tilo_delivery_protocol::workspace_join::DeliveryOriginLocatorV1::from_transport_hint(
                view.delivery_locator()
                    .ok_or("native delivery locator missing")?,
                view.host().ok_or("native delivery pin missing")?,
            )
            .map_err(|e| e.to_string())?;
            if descriptor.origin() != locator.origin {
                return Err(
                    "native descriptor differs from independently pinned discovery origin".into(),
                );
            }
            descriptor
                .client_scope(&authority, view.device(), Carrier::Https, 0, 0)
                .map_err(|e| e.to_string())?;
            let mut request = media_wire::BootstrapRequest {
                version: 1,
                device: view.device().device_public_key,
                nonce: media_wire::fresh_nonce().map_err(|e| e.to_string())?,
                workspace: view.workspace(),
                root: view.governor().user_public_key,
                room: room.to_string(),
                candidate_digest: descriptor.descriptor_digest(),
                signature: [[0; 32]; 2],
            };
            request.signature = media_wire::split_signature(
                key.sign(&request.signing_digest().map_err(|e| e.to_string())?)
                    .to_bytes(),
            );
            let mut buffer = vec![0_u8; descriptor::MAX_HINT_BYTES];
            let hint_bytes = postcard::to_slice(candidate, &mut buffer)
                .map_err(|e| e.to_string())?
                .to_vec();
            Ok(NativeBootstrap {
                owner: Arc::downgrade(&self.0),
                room,
                continuity: view.continuity(),
                request,
                descriptor,
                hint: candidate.clone(),
                hint_bytes,
            })
        })
    }
    /// Record the host's signed acknowledgement of `pending` as the room's
    /// descriptor floor.
    ///
    /// # Errors
    /// The response does not bind exactly to `pending` and the current room.
    pub fn finish_bootstrap(
        &self,
        pending: NativeBootstrap,
        response: &media_wire::BootstrapResponse,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let original = pending
            .owner
            .upgrade()
            .ok_or("native bootstrap owner expired")?;
        if !Arc::ptr_eq(&original, &self.0) {
            return Err("native bootstrap belongs to a different owner".into());
        }
        let mut candidate = self.with_current(pending.room, |view| {
            response.validate().map_err(|e| e.to_string())?;
            if view.continuity() != pending.continuity
                || view.device().device_public_key != pending.request.device
                || response.request != pending.request
                || response.hint != pending.hint_bytes
                || response.host_certificate
                    != postcard::to_stdvec(view.host().ok_or("native service pin missing")?)
                        .map_err(|e| e.to_string())?
                || response.generation != pending.descriptor.generation()
                || response.descriptor_digest != pending.descriptor.descriptor_digest()
                || response.issued_at_ns > enrollment::ns(now)?
                || response.expires_at_ns <= enrollment::ns(now)?
                || pending.request.workspace != view.workspace()
                || pending.request.root != view.governor().user_public_key
                || pending.request.room != view.peer().room()
                || view.peer().authority() != view.governor()
            {
                return Err("native bootstrap response binding mismatch".into());
            }
            pending
                .descriptor
                .revalidate(&view.descriptor_authority(now)?)
                .map_err(|e| e.to_string())?;
            VerifyingKey::from_bytes(&view.governor().device_public_key)
                .map_err(|e| e.to_string())?
                .verify_strict(
                    &response.signing_digest().map_err(|e| e.to_string())?,
                    &Signature::from_bytes(&media_wire::join_signature(response.signature)),
                )
                .map_err(|e| e.to_string())?;
            Ok(view.clone())
        })?;
        candidate.record.hint = Some(pending.hint);
        candidate.record.floor = Some(record::DescriptorFloor {
            generation: response.generation,
            digest: response.descriptor_digest,
        });
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_room(pending.room, candidate, None)
    }
}
