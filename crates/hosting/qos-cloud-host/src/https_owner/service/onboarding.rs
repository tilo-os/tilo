//! Device-possession onboarding for the existing Cloud Store join service.
//! This layer never issues invitations or admits membership itself: it proves
//! transport possession, invokes the Store's join operations in one reserved
//! turn, and signs the exact response (or a pinned handoff) with the host key.
use super::*;
use ed25519_dalek::Signer;
use qos_peer_state::join::{JoinTransportRequest, JoinTransportResponse};
use qos_relay::delivery_v3::onboarding_wire::{
    HANDOFF_CHUNKS_PATH, HandoffChunk, HandoffManifest, HandoffPurpose, HandoffRequest,
    HandoffSegment, MAX_HANDOFF_RANGE_BYTES, MAX_RESPONSE_BYTES, MAX_SEGMENT_BYTES,
    OnboardingOperation, OnboardingRequest, OnboardingResponse, OnboardingScope, ReplyBodyKind,
};
use qos_session_peer::ingress::ByteCredit;
use qos_user_identity::DeviceCert;

mod input;

const NONCE_LIMIT: usize = 16_384;
const PIN_NS: u64 = 30_000_000_000;
const PIN_LIMIT: usize = 8;
const INLINE_BYTES: usize = 1024 * 1024 - 8192;

#[derive(Debug)]
struct CapacityUnavailable;
impl std::fmt::Display for CapacityUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("onboarding capacity unavailable")
    }
}
impl std::error::Error for CapacityUnavailable {}
fn unavailable() -> anyhow::Error {
    CapacityUnavailable.into()
}
fn rejection_status(error: &anyhow::Error) -> StatusCode {
    if error.is::<CapacityUnavailable>() || transient(error) {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::FORBIDDEN
    }
}
/// A fixed, content-free category for an onboarding refusal log line.
fn refusal_reason(error: &anyhow::Error) -> &'static str {
    if error.is::<CapacityUnavailable>()
        || error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<CloudDeliveryError>(),
                Some(CloudDeliveryError::Capacity)
            )
        })
    {
        return "capacity";
    }
    if transient(error) {
        return "owner_busy";
    }
    let message = error.to_string();
    [
        ("expired", "expired"),
        ("nonce replay", "nonce_replay"),
        ("clock regressed", "clock_regressed"),
        ("descriptor", "descriptor_replaced"),
        ("revoked", "device_revoked"),
        ("another device", "device_mismatch"),
        ("route", "route_mismatch"),
        ("handoff", "handoff"),
        ("signature", "signature"),
        ("Store unavailable", "store_unavailable"),
    ]
    .into_iter()
    .find(|(needle, _)| message.contains(needle))
    .map_or("invalid", |(_, reason)| reason)
}
/// A content-free category for a Store-level join refusal.
fn join_refusal_reason(message: &str) -> &'static str {
    [
        ("not issued", "challenge_not_issued"),
        ("substitution", "request_id_substitution"),
        ("expired", "expired"),
        ("revoked", "revoked"),
        ("not the pinned structural owner", "not_owner"),
        ("mismatch", "scope_mismatch"),
        ("consumed", "invitation_consumed"),
        ("receipt", "receipt"),
    ]
    .into_iter()
    .find(|(needle, _)| message.contains(needle))
    .map_or("refused", |(_, reason)| reason)
}
struct SeenNonce {
    device: [u8; 32],
    nonce: [u8; 32],
    expires: u64,
}
struct Admission {
    certificate: DeviceCert,
    request: JoinTransportRequest,
    operation: OnboardingOperation,
    request_digest: [u8; 32],
    nonce: [u8; 32],
    expires: u64,
    descriptor: [u8; 32],
    _decode_credit: ByteCredit,
}
struct Pinned {
    manifest: Vec<u8>,
    payload: Vec<u8>,
    descriptor: [u8; 32],
    device: DeviceCert,
    expires: u64,
    next_offset: u64,
    last_range: Option<(u64, u32)>,
    // Both allocations and the certificate drop before their credits.
    _payload_credit: ByteCredit,
    _metadata_credit: ByteCredit,
}
pub(crate) struct Onboarding {
    boot: [u8; 16],
    high_water: u64,
    nonces: Vec<SeenNonce>,
    pins: Vec<Pinned>,
    budget: HostBytes,
    // Retained decoded caches drop before their exact capacity credit.
    _nonce_credit: ByteCredit,
    _pin_credit: ByteCredit,
}
fn at(now: u64) -> web_time::SystemTime {
    web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now)
}
/// Current certificate validity and revocation; membership for handoff reads.
fn current_device(
    turn: &CloudStoreTurn,
    certificate: &DeviceCert,
    now: u64,
    membership: bool,
) -> Result<()> {
    qos_user_identity::verify_device_cert_at(certificate, now)?;
    ensure!(
        !turn
            .peer()
            .revocations()
            .iter()
            .any(|entry| entry.revoked_device_public_key == certificate.device_public_key),
        "onboarding device revoked"
    );
    if membership {
        ensure!(
            turn.peer()
                .certificates()
                .iter()
                .any(|current| current == certificate),
            "handoff certificate no longer current"
        );
        turn.peer()
            .permissions(certificate.device_public_key, at(now))
            .map_err(anyhow::Error::msg)?;
    }
    Ok(())
}
fn scope<'a>(turn: &CloudStoreTurn, origin: &'a str) -> Result<OnboardingScope<'a>> {
    Ok(OnboardingScope {
        origin,
        host: turn.peer().authority().device_public_key,
        workspace: turn.workspace(),
        root: turn.peer().authority().user_public_key,
        room: turn.peer().room().parse()?,
    })
}

impl Onboarding {
    pub(crate) fn new(boot: [u8; 16], budget: HostBytes) -> Result<Self> {
        ensure!(boot != [0; 16], "onboarding boot unavailable");
        let nonce_credit = budget
            .try_acquire_bulk(NONCE_LIMIT * size_of::<SeenNonce>())
            .ok_or_else(unavailable)?;
        let mut nonces = Vec::new();
        nonces.try_reserve_exact(NONCE_LIMIT)?;
        let pin_credit = budget
            .try_acquire_bulk(PIN_LIMIT * size_of::<Pinned>())
            .ok_or_else(unavailable)?;
        let mut pins = Vec::new();
        pins.try_reserve_exact(PIN_LIMIT)?;
        Ok(Self {
            boot,
            high_water: 0,
            nonces,
            pins,
            budget,
            _nonce_credit: nonce_credit,
            _pin_credit: pin_credit,
        })
    }
    fn observe(&mut self, now: u64) -> Result<()> {
        ensure!(now >= self.high_water, "onboarding clock regressed");
        self.high_water = now;
        self.nonces.retain(|entry| entry.expires > now);
        self.pins.retain(|pin| pin.expires > now);
        Ok(())
    }

    /// Returns the untouched request when the Store was busy before the
    /// exchange took any effect (the caller defers and retries it).
    pub(super) fn handle(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        pins: &Pins,
        request: BufferedRequest,
    ) -> Option<BufferedRequest> {
        let result = match request.route() {
            Route::OnboardingChunk => self.chunk(source, pool, pins, &request),
            Route::Onboarding(_) => self.exchange(source, pool, pins, &request),
            _ => Err(anyhow!("not an onboarding route")),
        };
        match result {
            Ok(body) => {
                if let Err(error) = request.respond(StatusCode::OK, body) {
                    if let Some(suppressed) = ops_log::admit("onboarding_reply_unsent") {
                        tracing::warn!(
                            event = "cloud_onboarding_reply_unsent",
                            error,
                            suppressed,
                            "Cloud onboarding reply could not be sent"
                        );
                    }
                }
            }
            Err(error) if deferrable(&error) => return Some(request),
            Err(error) => {
                tracing::debug!(%error, "onboarding request refused");
                let reason = refusal_reason(&error);
                if let Some(suppressed) = ops_log::admit(reason) {
                    tracing::info!(
                        event = "cloud_onboarding_refused",
                        reason,
                        route = ?request.route(),
                        suppressed,
                        "Cloud onboarding request refused"
                    );
                }
                let _ = request.respond(rejection_status(&error), b"onboarding denied".to_vec());
            }
        }
        None
    }

    /// Admission, the Store join operation and the signed reply share one
    /// reserved turn; a join commit is refreshed before the reply is signed.
    fn exchange(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        pins: &Pins,
        request: &BufferedRequest,
    ) -> Result<Vec<u8>> {
        ensure!(
            request.method().as_str() == "POST",
            "onboarding requires POST"
        );
        with_turn(source, pool, DeliveryOperation::Onboarding, |turn| {
            let now = turn.fresh_now_ns()?;
            self.observe(now)?;
            let admission = self.verify(turn, pins, request.target(), request.body(), now)?;
            let Admission {
                certificate,
                request: join,
                operation,
                request_digest,
                nonce,
                expires,
                descriptor,
                _decode_credit,
            } = admission;
            let response = invoke(turn, certificate.device_public_key, join, now)?;
            let now = turn.fresh_now_ns()?;
            ensure!(now < expires, "onboarding proof expired during service");
            self.observe(now)?;
            self.reply(
                turn,
                pins,
                request.body(),
                &certificate,
                operation,
                request_digest,
                nonce,
                expires,
                descriptor,
                response,
                now,
            )
        })
    }

    fn verify(
        &mut self,
        turn: &CloudStoreTurn,
        pins: &Pins,
        target: &str,
        body: &[u8],
        now: u64,
    ) -> Result<Admission> {
        let wire = OnboardingRequest::decode(body)?;
        ensure!(
            wire.operation == OnboardingOperation::from_target("POST", target)?,
            "onboarding route mismatch"
        );
        let certificate = input::certificate(wire.certificate)?;
        let request = input::Request::decode(wire.body)?;
        let heap = certificate
            .heap()
            .checked_add(request.heap())
            .context("join allocation overflow")?;
        let credit = self.budget.try_acquire_bulk(heap).ok_or_else(unavailable)?;
        let _verification = self.budget.try_acquire_bulk(8192).ok_or_else(unavailable)?;
        let certificate = certificate.into_owned();
        current_device(turn, &certificate, now, false)?;
        let descriptor = turn.verified_descriptor(pins.capabilities)?;
        ensure!(
            descriptor.descriptor_digest() == pins.descriptor,
            "descriptor replaced"
        );
        wire.verify_device_signature(scope(turn, descriptor.origin())?, &certificate, now)?;
        if self.nonces.len() == NONCE_LIMIT {
            return Err(unavailable());
        }
        ensure!(
            !self
                .nonces
                .iter()
                .any(|entry| entry.device == certificate.device_public_key
                    && entry.nonce == wire.nonce),
            "onboarding nonce replay"
        );
        let request = request.into_owned();
        // The Store still makes the actual invitation/membership decision; this
        // only prevents substituting another transport identity.
        let same_device = match &request {
            JoinTransportRequest::Inspect { .. } => true,
            JoinTransportRequest::Challenge(v) => v.device_cert == certificate,
            JoinTransportRequest::Complete(v) => {
                v.proof.challenge.device_public_key == certificate.device_public_key
                    && v.proof.challenge.user_public_key == certificate.user_public_key
            }
            JoinTransportRequest::MembershipChallenge(v) => v.device_cert == certificate,
            JoinTransportRequest::MembershipComplete(v) => {
                v.proof.challenge.device_public_key == certificate.device_public_key
                    && v.proof.challenge.user_public_key == certificate.user_public_key
            }
        };
        ensure!(same_device, "join proof names another device");
        self.nonces.push(SeenNonce {
            device: certificate.device_public_key,
            nonce: wire.nonce,
            expires: wire.expires_at_ns,
        });
        Ok(Admission {
            certificate,
            request,
            operation: wire.operation,
            request_digest: wire.request_digest()?,
            nonce: wire.nonce,
            expires: wire.expires_at_ns,
            descriptor: descriptor.descriptor_digest(),
            _decode_credit: credit,
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "explicit protocol inputs stay separate parameters"
    )]
    fn reply(
        &mut self,
        turn: &CloudStoreTurn,
        pins: &Pins,
        original: &[u8],
        certificate: &DeviceCert,
        operation: OnboardingOperation,
        request_digest: [u8; 32],
        nonce: [u8; 32],
        expires: u64,
        descriptor_digest: [u8; 32],
        response: JoinTransportResponse,
        now: u64,
    ) -> Result<Vec<u8>> {
        let descriptor = turn.verified_descriptor(pins.capabilities)?;
        ensure!(
            descriptor.descriptor_digest() == descriptor_digest,
            "onboarding descriptor replaced"
        );
        current_device(turn, certificate, now, false)?;
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
                JoinTransportResponse::MembershipComplete(_)
                    | JoinTransportResponse::OwnerMembershipComplete(_),
                OnboardingOperation::MembershipComplete
            ) | (JoinTransportResponse::Error { .. }, _)
        );
        ensure!(valid, "onboarding response operation mismatch");
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
            postcard::to_slice(&response, &mut body)?.len() == length,
            "join response allocation changed"
        );
        let key = turn.store.key();
        let mut manifest_bytes = None;
        if length > INLINE_BYTES {
            ensure!(
                matches!(
                    response,
                    JoinTransportResponse::Complete(_)
                        | JoinTransportResponse::MembershipComplete(_)
                        | JoinTransportResponse::OwnerMembershipComplete(_)
                ),
                "large non-membership response unavailable"
            );
            current_device(turn, certificate, now, true)?;
            if self.pins.len() == PIN_LIMIT {
                return Err(unavailable());
            }
            // At most 64 segment entries + origin/manifest/certificate metadata.
            let metadata_credit = self
                .budget
                .try_acquire_bulk(24 * 1024)
                .ok_or_else(unavailable)?;
            let request = OnboardingRequest::decode(original)?;
            let pin_expires = now
                .checked_add(PIN_NS)
                .context("handoff expiry overflow")?
                .min(expires);
            let mut manifest = HandoffManifest {
                version: 1,
                purpose: HandoffPurpose::JoinResponse,
                scope: request.scope,
                device: certificate.device_public_key,
                request_digest,
                request_nonce: nonce,
                owner_generation: turn.generation(),
                server_boot_id: self.boot,
                issued_at_ns: now,
                expires_at_ns: pin_expires,
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
            manifest.signature =
                media_wire::split_signature(key.sign(&manifest.signing_digest()?).to_bytes());
            let encoded = manifest.encode()?;
            manifest_bytes = Some(encoded.clone());
            self.pins.push(Pinned {
                manifest: encoded,
                payload: std::mem::take(&mut body),
                descriptor: descriptor.descriptor_digest(),
                device: certificate.clone(),
                expires: pin_expires,
                next_offset: 0,
                last_range: None,
                _payload_credit: credit,
                _metadata_credit: metadata_credit,
            });
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
            request_digest,
            request_nonce: nonce,
            host: turn.peer().authority().device_public_key,
            server_boot_id: self.boot,
            issued_at_ns: now,
            expires_at_ns: now.saturating_add(PIN_NS).min(expires),
            body_digest: *blake3::hash(reply_body).as_bytes(),
            body: reply_body,
            signature: [[0; 32]; 2],
        };
        wire.signature = media_wire::split_signature(key.sign(&wire.signing_digest()?).to_bytes());
        Ok(exact_body(wire.encode()?))
    }

    fn chunk(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        pins: &Pins,
        request: &BufferedRequest,
    ) -> Result<Vec<u8>> {
        ensure!(
            request.method().as_str() == "POST" && request.target() == HANDOFF_CHUNKS_PATH,
            "invalid handoff route"
        );
        let body = request.body();
        with_turn(source, pool, DeliveryOperation::Replay, |turn| {
            let now = turn.fresh_now_ns()?;
            self.observe(now)?;
            let handoff = HandoffRequest::decode(body)?;
            let _scratch = self
                .budget
                .try_acquire_bulk(16 * 1024)
                .ok_or_else(unavailable)?;
            let mut index = None;
            for (i, pin) in self.pins.iter().enumerate() {
                if HandoffManifest::decode(&pin.manifest)?.manifest_digest()?
                    == handoff.manifest_digest
                {
                    index = Some(i);
                    break;
                }
            }
            let index = index.context("handoff is not retained")?;
            let manifest_bytes = self.pins[index].manifest.clone();
            let manifest = HandoffManifest::decode(&manifest_bytes)?;
            handoff.verify_device_signature(&manifest, now)?;
            let descriptor = turn.verified_descriptor(pins.capabilities)?;
            ensure!(
                descriptor.descriptor_digest() == self.pins[index].descriptor
                    && descriptor.origin() == manifest.scope.origin
                    && manifest.server_boot_id == self.boot
                    && turn.peer().authority().device_public_key == manifest.scope.host
                    && turn.workspace() == manifest.scope.workspace
                    && turn.peer().authority().user_public_key == manifest.scope.root
                    && turn.peer().room().parse::<uuid::Uuid>()? == manifest.scope.room
                    && turn.generation() >= manifest.owner_generation,
                "handoff host or scope changed"
            );
            current_device(turn, &self.pins[index].device, now, true)?;
            if self.nonces.len() == NONCE_LIMIT {
                return Err(unavailable());
            }
            ensure!(
                !self
                    .nonces
                    .iter()
                    .any(|entry| entry.device == handoff.device && entry.nonce == handoff.nonce),
                "handoff nonce replay"
            );
            let pin = &mut self.pins[index];
            let end = handoff
                .offset
                .checked_add(u64::from(handoff.length))
                .context("handoff range overflow")?;
            ensure!(
                handoff.length as usize <= MAX_HANDOFF_RANGE_BYTES
                    && end <= manifest.total_bytes
                    && (handoff.offset == pin.next_offset
                        || pin.last_range == Some((handoff.offset, handoff.length))),
                "handoff range out of order"
            );
            let bytes = pin
                .payload
                .get(handoff.offset as usize..end as usize)
                .context("handoff range exceeds pin")?;
            let mut chunk = HandoffChunk {
                version: 1,
                manifest_digest: handoff.manifest_digest,
                request_digest: handoff.request_digest()?,
                offset: handoff.offset,
                body_digest: *blake3::hash(bytes).as_bytes(),
                body: bytes,
                signature: [[0; 32]; 2],
            };
            chunk.signature = media_wire::split_signature(
                turn.store.key().sign(&chunk.signing_digest()?).to_bytes(),
            );
            let encoded = chunk.encode()?;
            pin.next_offset = pin.next_offset.max(end);
            pin.last_range = Some((handoff.offset, handoff.length));
            self.nonces.push(SeenNonce {
                device: handoff.device,
                nonce: handoff.nonce,
                expires: handoff.expires_at_ns,
            });
            // A fresh proof may retry the last range after a lost response.
            // Completed pins remain bounded until their <=30s expiry.
            Ok(encoded)
        })
    }
}

/// Invoke the actual Cloud Store join operation for the proven transport
/// device. A service refusal is a signed Error reply; an owner failure is not.
fn invoke(
    turn: &mut CloudStoreTurn,
    remote: [u8; 32],
    request: JoinTransportRequest,
    now: u64,
) -> Result<JoinTransportResponse> {
    use JoinTransportResponse as Response;
    let before = turn.generation();
    let result = match request {
        JoinTransportRequest::Inspect { signed_invitation } => turn
            .store
            .join_inspect(&signed_invitation, now)
            .map(Response::Inspection),
        JoinTransportRequest::Challenge(request) => turn
            .store
            .join_challenge(remote, request, now)
            .map(Response::Challenge),
        JoinTransportRequest::Complete(request) => turn
            .store
            .join_complete(remote, request, now)
            .and_then(|bootstrap| {
                turn.store
                    .verify_cached_membership(remote, &bootstrap, now)
                    .map(|()| Response::Complete(bootstrap))
            }),
        JoinTransportRequest::MembershipChallenge(request) => turn
            .store
            .membership_challenge(remote, request, now)
            .map(Response::MembershipChallenge),
        JoinTransportRequest::MembershipComplete(request) => turn
            .store
            .membership_refresh(remote, request, now)
            .map(crate::collaboration::MembershipRefresh::into_response),
    };
    if let Err(error) = &result {
        if !crate::is_store_failure(error) {
            let reason = join_refusal_reason(&error.to_string());
            if let Some(suppressed) = ops_log::admit(reason) {
                tracing::info!(
                    event = "cloud_onboarding_join_refused",
                    reason,
                    suppressed,
                    "Cloud Store refused an onboarding join operation"
                );
            }
        }
        if let Some(failure) = error.downcast_ref::<crate::StoreCommitFailure>() {
            let failure = *failure;
            turn.source.retire();
            return Err(CloudDeliveryError::OwnerUncertain(failure).into());
        }
        if crate::is_store_failure(error) {
            return Err(anyhow!("Cloud Store unavailable during onboarding"));
        }
    }
    if turn.generation() != before {
        turn.refresh_committed().map_err(delivery)?;
    }
    Ok(result.unwrap_or_else(|error| Response::Error {
        code: 403,
        message: error.to_string(),
    }))
}

#[cfg(test)]
mod reply_capacity_tests {
    use super::*;

    /// The HTTPS carrier's response bound (qos-session-peer `BODY_BYTES`),
    /// which it checks against the body's allocation capacity.
    const CARRIER_BODY_BYTES: usize = 1024 * 1024;

    #[test]
    fn large_inline_membership_reply_fits_the_carrier_capacity_bound() {
        // An inline membership reply just under the handoff threshold: the
        // size a grown workspace's bootstrap reaches.
        let mut body = vec![6u8; INLINE_BYTES];
        body[0] = 6; // OwnerMembershipComplete discriminant
        let wire = OnboardingResponse {
            version: 3,
            operation: OnboardingOperation::MembershipComplete,
            body_kind: ReplyBodyKind::InlineLegacy,
            request_digest: [1; 32],
            request_nonce: [2; 32],
            host: [3; 32],
            server_boot_id: [4; 16],
            issued_at_ns: 1_000,
            expires_at_ns: 2_000,
            body_digest: *blake3::hash(&body).as_bytes(),
            body: &body,
            signature: [[5; 32]; 2],
        };
        let encoded = wire.encode().unwrap();
        assert!(encoded.len() <= CARRIER_BODY_BYTES);
        // The encoder's own allocation would be refused by the carrier.
        assert!(encoded.capacity() > CARRIER_BODY_BYTES);
        let sent = exact_body(encoded);
        assert_eq!(sent.capacity(), sent.len());
        assert!(sent.capacity() <= CARRIER_BODY_BYTES);
        assert_eq!(OnboardingResponse::decode(&sent).unwrap().body, &body[..]);
    }
}
