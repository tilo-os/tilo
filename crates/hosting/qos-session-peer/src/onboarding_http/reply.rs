//! Bounded signed responses and temporary, charged onboarding response backing.
use super::payload::Payload;
use super::*;
use ed25519_dalek::Signer;
use qos_peer_state::join::JoinTransportResponse;
use tilo_relay::delivery_v3::{
    media_wire::split_signature,
    onboarding_wire::{
        HANDOFF_CHUNKS_PATH, HandoffChunk, HandoffManifest, HandoffPurpose, HandoffRequest,
        HandoffSegment, MAX_HANDOFF_RANGE_BYTES, MAX_RESPONSE_BYTES, MAX_SEGMENT_BYTES,
        OnboardingResponse, ReplyBodyKind,
    },
};
const PIN_NS: u64 = 30_000_000_000;
const INLINE_BYTES: usize = 1024 * 1024 - 8192;

pub(super) struct Pinned {
    pub(super) manifest: Vec<u8>,
    pub(super) payload: Payload,
    pub(super) descriptor: [u8; 32],
    pub(super) device: DeviceCert,
    pub(super) expires: u64,
    pub(super) next_offset: u64,
    pub(super) last_range: Option<(u64, u32)>,
    // Both allocations and the certificate drop before their credits.
    pub(super) _payload_credit: Option<ByteCredit>,
    pub(super) _metadata_credit: ByteCredit,
}
impl OnboardingRegistry {
    /// Called only after the real owner barrier and membership reconciliation.
    /// The host retains its OwnerScope until this consumes the original exchange.
    pub(crate) fn finish(
        &mut self,
        exchange: OnboardingExchange,
        owner: &SessionOwner,
        view: &CommittedView,
        response: JoinTransportResponse,
        now: u64,
    ) -> Result<()> {
        ensure!(now >= self.high_water, "onboarding clock regressed");
        self.high_water = now;
        ensure!(!exchange.is_cancelled(), "onboarding response cancelled");
        ensure!(
            exchange.admission.request.is_none(),
            "onboarding service was not invoked"
        );
        ensure!(
            now < exchange.admission.expires,
            "onboarding proof expired during service"
        );
        let descriptor = owner.verified_delivery_transport(view, at(now), PRODUCT_DATA_V1)?;
        ensure!(
            descriptor.descriptor_digest() == exchange.admission.descriptor_digest,
            "onboarding descriptor replaced"
        );
        current_device(owner, &exchange.admission.certificate, now, false)?;
        let length = postcard::experimental::serialized_size(&response)?;
        ensure!(
            length > 0 && length <= MAX_RESPONSE_BYTES,
            "join response exceeds frozen bound"
        );
        let credit = self
            .budget
            .try_acquire_bulk(length)
            .ok_or_else(unavailable)?;
        let mut body = vec![0; length];
        ensure!(
            postcard::to_slice(&response, &mut body)?.len() == length && body.capacity() == length,
            "join response allocation changed"
        );
        let operation = exchange.admission.operation;
        let valid = matches!(
            (&response, operation),
            (
                JoinTransportResponse::Inspection(_),
                OnboardingOperation::Inspect
            ) | (
                JoinTransportResponse::Challenge(_),
                OnboardingOperation::Challenge
            ) | (
                JoinTransportResponse::Complete(_),
                OnboardingOperation::Complete
            ) | (
                JoinTransportResponse::MembershipChallenge(_),
                OnboardingOperation::MembershipChallenge
            ) | (
                JoinTransportResponse::MembershipComplete(_),
                OnboardingOperation::MembershipComplete
            ) | (JoinTransportResponse::Error { .. }, _)
        );
        ensure!(valid, "onboarding response operation mismatch");
        let mut manifest_bytes = None;
        if length > INLINE_BYTES {
            ensure!(
                matches!(
                    response,
                    JoinTransportResponse::Complete(_)
                        | JoinTransportResponse::MembershipComplete(_)
                ),
                "large non-membership response unavailable"
            );
            current_device(owner, &exchange.admission.certificate, now, true)?;
            self.pins.retain(|pin| pin.expires > now);
            if self.pins.len() == 8 {
                return Err(unavailable());
            }
            // At most64 segment entries + origin/manifest/certificate metadata.
            let metadata_credit = self
                .budget
                .try_acquire_bulk(24 * 1024)
                .ok_or_else(unavailable)?;
            let request = OnboardingRequest::decode(exchange.request.body())?;
            let expires = now
                .checked_add(PIN_NS)
                .ok_or_else(|| anyhow::anyhow!("handoff expiry overflow"))?
                .min(exchange.admission.expires);
            let mut manifest = HandoffManifest {
                version: 1,
                purpose: HandoffPurpose::JoinResponse,
                scope: request.scope,
                device: exchange.admission.certificate.device_public_key,
                request_digest: exchange.admission.request_digest,
                request_nonce: exchange.admission.nonce,
                owner_generation: owner.committed_generation(),
                server_boot_id: self.boot,
                issued_at_ns: now,
                expires_at_ns: expires,
                total_bytes: length as u64,
                content_digest: *blake3::hash(&body).as_bytes(),
                segments: body
                    .chunks(MAX_SEGMENT_BYTES as usize)
                    .map(|bytes| HandoffSegment {
                        digest: *blake3::hash(bytes).as_bytes(),
                        length: bytes.len() as u64,
                    })
                    .collect(),
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
            manifest_bytes = Some(encoded.clone());
            self.pins.push(Pinned {
                manifest: encoded,
                payload: Payload::Encoded(body),
                descriptor: descriptor.descriptor_digest(),
                device: exchange.admission.certificate.clone(),
                expires,
                next_offset: 0,
                last_range: None,
                _payload_credit: Some(credit),
                _metadata_credit: metadata_credit,
            });
            body = Vec::new();
        }
        let reply_body = manifest_bytes.as_deref().unwrap_or(&body);
        let mut wire = OnboardingResponse {
            version: 3,
            operation,
            body_kind: if manifest_bytes.is_some() {
                ReplyBodyKind::HandoffManifest
            } else {
                ReplyBodyKind::InlineLegacy
            },
            request_digest: exchange.admission.request_digest,
            request_nonce: exchange.admission.nonce,
            host: owner.host_certificate().device_public_key,
            server_boot_id: self.boot,
            issued_at_ns: now,
            expires_at_ns: now.saturating_add(PIN_NS).min(exchange.admission.expires),
            body_digest: *blake3::hash(reply_body).as_bytes(),
            body: reply_body,
            signature: [[0; 32]; 2],
        };
        wire.signature =
            split_signature(owner.identity.host.sign(&wire.signing_digest()?).to_bytes());
        let mut encoded = wire.encode()?;
        // The HTTPS reply bound charges capacity; postcard's grown Vec can
        // exceed 1 MiB for a reply whose encoded length fits.
        encoded.shrink_to_fit();
        exchange
            .request
            .respond(hyper::StatusCode::OK, encoded)
            .map_err(anyhow::Error::msg)
    }

    pub(crate) fn chunk(
        &mut self,
        owner: &SessionOwner,
        view: &CommittedView,
        request: crate::https::BufferedRequest,
        now: u64,
    ) -> Result<()> {
        ensure!(!request.is_cancelled(), "onboarding chunk cancelled");
        let result = self.chunk_bytes(
            owner,
            view,
            request.method().as_str(),
            request.target(),
            request.body(),
            now,
        );
        match result {
            Ok(bytes) => request
                .respond(hyper::StatusCode::OK, bytes)
                .map_err(anyhow::Error::msg),
            Err(error) => {
                let _ = request.respond(
                    rejection_status(&error),
                    b"onboarding handoff denied".to_vec(),
                );
                Err(error)
            }
        }
    }
    fn chunk_bytes(
        &mut self,
        owner: &SessionOwner,
        view: &CommittedView,
        method: &str,
        target: &str,
        body: &[u8],
        now: u64,
    ) -> Result<Vec<u8>> {
        ensure!(
            method == "POST" && target == HANDOFF_CHUNKS_PATH,
            "invalid handoff route"
        );
        ensure!(now >= self.high_water, "onboarding clock regressed");
        self.high_water = now;
        self.pins.retain(|pin| pin.expires > now);
        self.nonces.retain(|entry| entry.expires > now);
        let request = HandoffRequest::decode(body)?;
        let _scratch = self
            .budget
            .try_acquire_bulk(16 * 1024)
            .ok_or_else(unavailable)?;
        let mut index = None;
        for (i, pin) in self.pins.iter().enumerate() {
            if HandoffManifest::decode(&pin.manifest)?.manifest_digest()? == request.manifest_digest
            {
                index = Some(i);
                break;
            }
        }
        let pin =
            &mut self.pins[index.ok_or_else(|| anyhow::anyhow!("handoff is not retained"))?];
        let manifest = HandoffManifest::decode(&pin.manifest)?;
        request.verify_device_signature(&manifest, now)?;
        let descriptor = owner.verified_delivery_transport(view, at(now), PRODUCT_DATA_V1)?;
        ensure!(
            descriptor.descriptor_digest() == pin.descriptor
                && descriptor.origin() == manifest.scope.origin
                && manifest.server_boot_id == self.boot
                && owner.host_certificate().device_public_key == manifest.scope.host
                && owner.workspace_id() == manifest.scope.workspace
                && owner.peer().authority().user_public_key == manifest.scope.root
                && owner.peer().room().parse::<uuid::Uuid>()? == manifest.scope.room
                && owner.committed_generation() >= manifest.owner_generation,
            "handoff host or scope changed"
        );
        current_device(owner, &pin.device, now, true)?;
        if self.nonces.len() == NONCE_LIMIT {
            return Err(unavailable());
        }
        ensure!(
            !self
                .nonces
                .iter()
                .any(|entry| entry.device == request.device && entry.nonce == request.nonce),
            "handoff nonce replay"
        );
        let end = request
            .offset
            .checked_add(u64::from(request.length))
            .ok_or_else(|| anyhow::anyhow!("handoff range overflow"))?;
        ensure!(
            request.length as usize <= MAX_HANDOFF_RANGE_BYTES
                && end <= manifest.total_bytes
                && (request.offset == pin.next_offset
                    || pin.last_range == Some((request.offset, request.length))),
            "handoff range out of order"
        );
        let bytes = pin
            .payload
            .range(request.offset as usize, request.length as usize)?;
        let mut chunk = HandoffChunk {
            version: 1,
            manifest_digest: request.manifest_digest,
            request_digest: request.request_digest()?,
            offset: request.offset,
            body_digest: *blake3::hash(&bytes).as_bytes(),
            body: &bytes,
            signature: [[0; 32]; 2],
        };
        chunk.signature = split_signature(
            owner
                .identity
                .host
                .sign(&chunk.signing_digest()?)
                .to_bytes(),
        );
        let encoded = chunk.encode()?;
        self.nonces.push(SeenNonce {
            device: request.device,
            nonce: request.nonce,
            expires: request.expires_at_ns,
        });
        pin.next_offset = pin.next_offset.max(end);
        pin.last_range = Some((request.offset, request.length));
        // A fresh proof may retry the last range after a lost response. Completed
        // pins remain bounded until their <=30s expiry; live pins are not evicted.
        Ok(encoded)
    }
}
pub(super) fn current_device(
    owner: &SessionOwner,
    certificate: &DeviceCert,
    now: u64,
    membership: bool,
) -> Result<()> {
    tilo_user_identity::verify_device_cert_at(certificate, now)?;
    ensure!(
        !owner
            .peer()
            .revocations()
            .iter()
            .any(|entry| entry.revoked_device_public_key == certificate.device_public_key),
        "handoff device revoked"
    );
    if membership {
        ensure!(
            owner
                .peer()
                .certificates()
                .iter()
                .any(|current| current == certificate),
            "handoff certificate no longer current"
        );
        owner
            .peer()
            .permissions(certificate.device_public_key, at(now))
            .map_err(anyhow::Error::msg)?;
    }
    Ok(())
}
