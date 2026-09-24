//! Shared private delivery bootstrap and current Peer authority.
//! Wire values never substitute for the held key, governed state or storage custody.
use ed25519_dalek::{Signer, SigningKey};
use qos_peer_state::StatePeer;
use std::collections::BTreeMap;
use tilo_relay::delivery_v3::{
    auth::{self, Carrier},
    capabilities,
    descriptor::{self, VerifiedDescriptor},
    http_proof, media_wire,
};
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::{Duration, SystemTime};

/// Browser wall-clock resolution (Date.now is milliseconds, possibly coarsened).
const CLOCK_RESOLUTION_NS: u64 = 2_000_000;
/// Borrowed only from the live Peer and its acknowledged descriptor floor.
/// No field is supplied by a JS proof/identity object.
pub(crate) struct OwnerView<'a> {
    pub peer: &'a StatePeer,
    pub key: &'a SigningKey,
    pub governor: &'a DeviceCert,
    pub host: &'a DeviceCert,
    pub device: &'a DeviceCert,
    pub workspace: u64,
    pub lease_epoch: &'a str,
    pub generation: descriptor::GenerationExpectation,
    pub now_ns: u64,
    /// The live Peer's committed-state revision (`Peer::state_revision`, which
    /// every StatePeer replacement advances). Zero means unknown and disables
    /// verification reuse (see `VerificationMemo`).
    pub state_revision: u64,
}
impl OwnerView<'_> {
    /// An enrollment refresh replaces only an inert candidate, never the pins.
    pub fn validate_enrollment_hint(
        &self,
        workspace: u64,
        room: &str,
        governor: &DeviceCert,
        host: &DeviceCert,
        device: &DeviceCert,
        hint: &qos_peer_state::join::TransportHint,
    ) -> Result<(), String> {
        if workspace != self.workspace
            || room != self.peer.room()
            || governor != self.governor
            || host != self.host
            || device != self.device
        {
            return Err("media enrollment custody mismatch".into());
        }
        let descriptor =
            descriptor::verify_transport_hint(hint, &self.descriptor_authority()).map_err(fail)?;
        descriptor
            .client_scope(
                &self.descriptor_authority(),
                self.device,
                Carrier::Https,
                0,
                0,
            )
            .map_err(fail)?;
        Ok(())
    }

    pub(crate) fn descriptor_authority(&self) -> descriptor::DescriptorAuthority<'_> {
        descriptor::DescriptorAuthority {
            peer: self.peer,
            pinned_governor: self.governor,
            pinned_host: self.host,
            workspace: self.workspace,
            generation: self.generation,
            allowed_delivery_capabilities: capabilities::PRODUCT_DATA_V1,
            now: time(self.now_ns),
        }
    }
    pub(crate) fn revocations(&self) -> Result<RevocationList, String> {
        let mut revoked = RevocationList::new();
        for entry in self.peer.revocations() {
            revoked.insert(entry).map_err(|e| e.to_string())?;
        }
        Ok(revoked)
    }
}
impl auth::Authority for OwnerView<'_> {
    fn authorize(
        &self,
        binding: &auth::Binding,
        device: &DeviceCert,
        host: &DeviceCert,
        now_ns: u64,
    ) -> Result<(), auth::AuthError> {
        if self.peer.incomplete_reason().is_some()
            || self.peer.authority() != self.governor
            || binding.workspace != self.workspace
            || binding.workspace_root != self.governor.user_public_key
            || device != self.device
            || host != self.host
            || !host.capabilities.can_act_as_host
            || !self.peer.certificates().contains(device)
            || !self.peer.certificates().contains(host)
            || self
                .peer
                .permissions(device.device_public_key, time(now_ns))
                .is_err()
            || self
                .peer
                .permissions(host.device_public_key, time(now_ns))
                .is_err()
        {
            return Err(auth::AuthError::PolicyDenied);
        }
        Ok(())
    }
}
pub(crate) fn time(ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_nanos(ns)
}
pub(crate) fn fail(message: impl ToString) -> String {
    message.to_string()
}

/// First-contact exception: only a typed enrolled-device bootstrap request is signed.
/// No media intent or delivery session exists until the returned floor is ACKed.
pub(crate) struct Bootstrap {
    lease_epoch: String,
    request: media_wire::BootstrapRequest,
    descriptor: VerifiedDescriptor,
    hint: Vec<u8>,
}
pub(crate) struct VerifiedBootstrap {
    pub descriptor: VerifiedDescriptor,
    pub floor: crate::peer_journal::DescriptorFloorV1,
}
impl Bootstrap {
    pub fn start(
        view: &OwnerView<'_>,
        hint: &qos_peer_state::join::TransportHint,
    ) -> Result<(Self, PreparedRequest), String> {
        if view.lease_epoch.is_empty()
            || view.lease_epoch.len() > 128
            || view.key.verifying_key().to_bytes() != view.device.device_public_key
        {
            return Err("bootstrap custody unavailable".into());
        }
        // Verification here is a preflight for endpoint/certificate safety only.
        // First-contact generation cannot become ordinary signing authority until
        // the governor response and durable journal acknowledgement below.
        let descriptor =
            descriptor::verify_transport_hint(hint, &view.descriptor_authority()).map_err(fail)?;
        descriptor
            .client_scope(
                &view.descriptor_authority(),
                view.device,
                Carrier::Https,
                0,
                0,
            )
            .map_err(fail)?;
        let mut request = media_wire::BootstrapRequest {
            version: 1,
            device: view.device.device_public_key,
            nonce: media_wire::fresh_nonce().map_err(fail)?,
            workspace: view.workspace,
            root: view.governor.user_public_key,
            room: view.peer.room().to_owned(),
            candidate_digest: descriptor.descriptor_digest(),
            signature: [[0; 32]; 2],
        };
        request.signature = media_wire::split_signature(
            view.key
                .sign(&request.signing_digest().map_err(fail)?)
                .to_bytes(),
        );
        let prepared = PreparedRequest {
            id: 0,
            origin: descriptor.origin().to_owned(),
            target: "/peer/v3/bootstrap",
            body: request.encode().map_err(fail)?,
            proof: None,
        };
        let mut buffer = [0; descriptor::MAX_HINT_BYTES];
        let hint = postcard::to_slice(hint, &mut buffer)
            .map_err(fail)?
            .to_vec();
        Ok((
            Self {
                lease_epoch: view.lease_epoch.into(),
                request,
                descriptor,
                hint,
            },
            prepared,
        ))
    }
    pub fn finish(self, view: &OwnerView<'_>, bytes: &[u8]) -> Result<VerifiedBootstrap, String> {
        if view.lease_epoch != self.lease_epoch
            || view.key.verifying_key().to_bytes() != self.request.device
        {
            return Err("bootstrap custody changed".into());
        }
        let response = media_wire::BootstrapResponse::decode(bytes).map_err(fail)?;
        if response.request != self.request
            || response.hint != self.hint
            || response.host_certificate != postcard::to_stdvec(view.host).map_err(fail)?
            || response.generation != self.descriptor.generation()
            || response.descriptor_digest != self.descriptor.descriptor_digest()
            || response.issued_at_ns > view.now_ns.saturating_add(CLOCK_RESOLUTION_NS)
            || response.expires_at_ns <= view.now_ns
            || self.request.workspace != view.workspace
            || self.request.root != view.governor.user_public_key
            || self.request.room != view.peer.room()
            || view.peer.authority() != view.governor
        {
            return Err("bootstrap response binding mismatch".into());
        }
        self.descriptor
            .revalidate(&view.descriptor_authority())
            .map_err(fail)?;
        ed25519_dalek::VerifyingKey::from_bytes(&view.governor.device_public_key)
            .map_err(fail)?
            .verify_strict(
                &response.signing_digest().map_err(fail)?,
                &ed25519_dalek::Signature::from_bytes(&media_wire::join_signature(
                    response.signature,
                )),
            )
            .map_err(fail)?;
        let floor = crate::peer_journal::DescriptorFloorV1 {
            version: 1,
            identity: crate::peer_delivery_cursor::CursorIdentity {
                workspace: view.workspace,
                room: view.peer.room().to_owned(),
                workspace_root: view.governor.user_public_key,
                device: view.device.device_public_key,
                device_root: view.device.user_public_key,
                host: view.host.device_public_key,
                host_root: view.host.user_public_key,
            },
            generation: response.generation,
            descriptor_digest: response.descriptor_digest,
        };
        floor.validate()?;
        Ok(VerifiedBootstrap {
            descriptor: self.descriptor,
            floor,
        })
    }
}

/// Prepared exact bytes only. JS cannot choose a signing preimage or identity.
pub(crate) struct PreparedRequest {
    pub id: u32,
    pub origin: String,
    pub target: &'static str,
    pub body: Vec<u8>,
    pub proof: Option<Vec<u8>>,
}

pub(crate) struct HttpSession {
    pub(crate) proof: auth::ServerProof,
    pub(crate) nonce: [u8; 32],
    pub(crate) digest: [u8; 32],
}
enum Pending {
    Server {
        nonce: [u8; 32],
    },
    Ready {
        proof: auth::ServerProof,
        nonce: [u8; 32],
        digest: [u8; 32],
    },
}
pub(crate) enum HttpStep {
    Request(PreparedRequest),
    Ready,
}
pub(crate) struct DeliveryAuthority {
    lease_epoch: String,
    pub(crate) descriptor: VerifiedDescriptor,
    floor: crate::peer_journal::AcknowledgedDescriptorFloor,
    pending: BTreeMap<u32, Pending>,
    next: u32,
    pub(crate) session: Option<HttpSession>,
    closed: bool,
    required_capabilities: u64,
    /// Last successful descriptor revalidation + client scope (`check`).
    checked: std::cell::Cell<Option<VerificationMemo>>,
    /// Last successful host-proof verification of the held session (`prove`).
    proved: std::cell::Cell<Option<VerificationMemo>>,
}
/// How long a successful signature/descriptor verification is reused for the
/// same committed Peer state. Every delivery request re-verified the
/// descriptor hint and host certificates several times (automerge governance
/// reads, JSON certificate parsing, Ed25519) on the main thread: about a
/// quarter of all runtime time while typing. Any committed-state change
/// (revocation, roster, governance, admission) advances the revision and
/// forces full verification; within one revision only time-based expiry can
/// change the outcome, so reuse is bounded to one second. The native host
/// verifies every request proof independently.
const VERIFICATION_REUSE_NS: u64 = 1_000_000_000;
#[derive(Clone, Copy, PartialEq, Eq)]
struct VerificationMemo {
    state_revision: u64,
    subject: [u8; 32],
    capabilities: u64,
    generation: (u8, u64, [u8; 32]),
    verified_at_ns: u64,
}
impl VerificationMemo {
    fn new(view: &OwnerView<'_>, subject: [u8; 32], capabilities: u64) -> Option<Self> {
        (view.state_revision != 0).then(|| Self {
            state_revision: view.state_revision,
            subject,
            capabilities,
            generation: match view.generation {
                descriptor::GenerationExpectation::FirstAccepted { minimum_generation } => {
                    (0, minimum_generation, [0; 32])
                }
                descriptor::GenerationExpectation::Current {
                    generation,
                    descriptor_digest,
                } => (1, generation, descriptor_digest),
            },
            verified_at_ns: view.now_ns,
        })
    }
    fn reusable(memo: Option<Self>, current: Option<Self>) -> bool {
        match (memo, current) {
            (Some(memo), Some(current)) => {
                memo.state_revision == current.state_revision
                    && memo.subject == current.subject
                    && memo.capabilities == current.capabilities
                    && memo.generation == current.generation
                    && current.verified_at_ns >= memo.verified_at_ns
                    && current.verified_at_ns - memo.verified_at_ns < VERIFICATION_REUSE_NS
            }
            _ => false,
        }
    }
}
impl DeliveryAuthority {
    pub fn new(
        lease: &str,
        descriptor: VerifiedDescriptor,
        floor: crate::peer_journal::AcknowledgedDescriptorFloor,
    ) -> Result<Self, String> {
        if lease.is_empty() || lease.len() > 128 {
            return Err(fail("invalid media custody lease"));
        }
        if descriptor.generation() != floor.floor().generation
            || descriptor.descriptor_digest() != floor.floor().descriptor_digest
        {
            return Err("descriptor was not durably acknowledged".into());
        }
        Ok(Self {
            lease_epoch: lease.into(),
            descriptor,
            floor,
            pending: BTreeMap::new(),
            next: 0,
            session: None,
            closed: false,
            required_capabilities: 0,
            checked: std::cell::Cell::new(None),
            proved: std::cell::Cell::new(None),
        })
    }
    pub(crate) fn check(&self, view: &OwnerView<'_>) -> Result<(), String> {
        if self.closed
            || view.lease_epoch != self.lease_epoch
            || view.key.verifying_key().to_bytes() != view.device.device_public_key
        {
            return Err(fail("media custody unavailable"));
        }
        let identity = &self.floor.floor().identity;
        if identity.workspace != view.workspace
            || identity.room != view.peer.room()
            || identity.workspace_root != view.governor.user_public_key
            || identity.device != view.device.device_public_key
            || identity.device_root != view.device.user_public_key
            || identity.host != view.host.device_public_key
            || identity.host_root != view.host.user_public_key
        {
            return Err("acknowledged descriptor custody mismatch".into());
        }
        let memo = VerificationMemo::new(
            view,
            view.device.device_public_key,
            self.required_capabilities,
        );
        if VerificationMemo::reusable(self.checked.get(), memo) {
            return Ok(());
        }
        self.checked.set(None);
        self.descriptor
            .revalidate(&view.descriptor_authority())
            .map_err(fail)?;
        self.descriptor
            .client_scope(
                &view.descriptor_authority(),
                view.device,
                Carrier::Https,
                self.requested_capabilities(),
                self.required_capabilities,
            )
            .map_err(fail)?;
        self.checked.set(memo);
        Ok(())
    }
    fn request(
        &mut self,
        target: &'static str,
        body: Vec<u8>,
        proof: Option<Vec<u8>>,
        pending: Pending,
    ) -> Result<PreparedRequest, String> {
        if self.pending.len() >= 8 {
            return Err(fail("media control capacity"));
        }
        let id = self
            .next
            .checked_add(1)
            .ok_or("media operation counter exhausted")?;
        self.next = id;
        self.pending.insert(id, pending);
        Ok(PreparedRequest {
            id,
            origin: self.descriptor.origin().into(),
            target,
            body,
            proof,
        })
    }
    pub fn start_product_http(&mut self, view: &OwnerView<'_>) -> Result<PreparedRequest, String> {
        if self.session.is_some() || !self.pending.is_empty() {
            return Err("delivery HTTP handshake already active".into());
        }
        self.required_capabilities = capabilities::PRODUCT_DATA_V1;
        self.start_http(view)
    }
    pub fn product_ready(&self, view: &OwnerView<'_>) -> bool {
        self.current(view)
            && self.session.as_ref().is_some_and(|session| {
                session.proof.transcript.negotiated_capabilities & capabilities::PRODUCT_DATA_V1
                    == capabilities::PRODUCT_DATA_V1
            })
    }
    /// Required bits plus the optional transcript extensions (multi-group
    /// replay) whenever the durable replay lane is required. Old hosts
    /// negotiate the optional bits away.
    fn requested_capabilities(&self) -> u64 {
        capabilities::handshake_policy(self.required_capabilities)
    }
    /// Whether this session's host may answer a poll with several groups.
    pub fn multi_group_replay(&self) -> bool {
        self.session.as_ref().is_some_and(|session| {
            session.proof.transcript.negotiated_capabilities & capabilities::MULTI_GROUP_REPLAY_V1
                != 0
        })
    }
    pub fn close(&mut self) {
        self.closed = true;
        self.session = None;
        self.pending.clear();
        self.checked.set(None);
        self.proved.set(None);
    }
    pub fn start_http(&mut self, view: &OwnerView<'_>) -> Result<PreparedRequest, String> {
        self.check(view)?;
        if self.session.is_some() || !self.pending.is_empty() {
            return Err(fail("media HTTP handshake already active"));
        }
        let nonce = media_wire::fresh_nonce().map_err(fail)?;
        let body = postcard::to_stdvec(&(
            3_u16,
            view.device.device_public_key,
            nonce,
            self.requested_capabilities(),
            self.required_capabilities,
        ))
        .map_err(fail)?;
        self.request("/peer/v3/challenge", body, None, Pending::Server { nonce })
    }
    pub(crate) fn verify_host(
        &self,
        view: &OwnerView<'_>,
        proof: &auth::ServerProof,
        nonce: [u8; 32],
    ) -> Result<auth::VerifiedHost, String> {
        let revoked = view.revocations()?;
        let scope = self
            .descriptor
            .client_scope(
                &view.descriptor_authority(),
                view.device,
                Carrier::Https,
                self.requested_capabilities(),
                self.required_capabilities,
            )
            .map_err(fail)?;
        auth::verify_server(
            &auth::ClientExpectations {
                scope,
                capabilities: auth::CapabilityPolicy {
                    supported: capabilities::KNOWN_DATA,
                    // Transcript extensions are optional and never in the
                    // signed descriptor; negotiation still intersects.
                    allowed: capabilities::client_allowed(self.descriptor.delivery_capabilities()),
                },
                credentials: auth::Credentials {
                    device: view.device,
                    host: view.host,
                    device_revocations: &revoked,
                    host_revocations: &revoked,
                },
                authority: view,
                // The host stamps nanoseconds; the browser clock has millisecond
                // resolution. Verify at the end of the current browser tick so
                // a reply issued within the same millisecond is not "future".
                now_ns: view.now_ns.saturating_add(CLOCK_RESOLUTION_NS),
            },
            nonce,
            proof,
        )
        .map_err(fail)
    }
    pub fn advance(
        &mut self,
        view: &OwnerView<'_>,
        id: u32,
        bytes: &[u8],
    ) -> Result<HttpStep, String> {
        self.check(view)?;
        let pending = self
            .pending
            .remove(&id)
            .ok_or("unknown or consumed media operation")?;
        if bytes.len() > 8192 {
            return Err(fail("oversized media response"));
        }
        match pending {
            Pending::Server { nonce } => {
                let proof = auth::ServerProof::decode(bytes).map_err(fail)?;
                let verified = self.verify_host(view, &proof, nonce)?;
                let transcript = &proof.transcript;
                let digest = http_proof::session_binding_digest(
                    &transcript.binding,
                    transcript.session_id,
                    transcript.negotiated_capabilities,
                    transcript.expires_at_ns,
                )
                .map_err(fail)?;
                let body = verified
                    .client_proof(view.key)
                    .map_err(fail)?
                    .encode()
                    .map_err(fail)?;
                Ok(HttpStep::Request(self.request(
                    "/peer/v3/sessions",
                    body,
                    None,
                    Pending::Ready {
                        proof,
                        nonce,
                        digest,
                    },
                )?))
            }
            Pending::Ready {
                proof,
                nonce,
                digest,
            } => {
                self.verify_host(view, &proof, nonce)?;
                let actual: (u16, [u8; 16], [u8; 32], u64, u64) =
                    postcard::from_bytes(bytes).map_err(fail)?;
                let expected = (
                    3,
                    proof.transcript.session_id,
                    digest,
                    proof.transcript.expires_at_ns,
                    proof.transcript.negotiated_capabilities,
                );
                if actual != expected || postcard::to_stdvec(&actual).map_err(fail)? != bytes {
                    return Err(fail("media HTTP ready binding mismatch"));
                }
                self.session = Some(HttpSession {
                    proof,
                    nonce,
                    digest,
                });
                Ok(HttpStep::Ready)
            }
        }
    }
    pub fn forget(&mut self, id: u32) {
        self.pending.remove(&id);
    }
    pub fn current(&self, view: &OwnerView<'_>) -> bool {
        self.check(view).is_ok()
            && self
                .session
                .as_ref()
                .is_some_and(|session| view.now_ns < session.proof.transcript.expires_at_ns)
    }
    pub fn http_ready(&self) -> bool {
        !self.closed && self.session.is_some()
    }
    pub(crate) fn prove(
        &self,
        view: &OwnerView<'_>,
        method: &str,
        target: &str,
        body: &[u8],
    ) -> Result<Vec<u8>, String> {
        self.check(view)?;
        let session = self
            .session
            .as_ref()
            .ok_or("delivery HTTP handshake required")?;
        // The held session proof was fully verified at handshake; re-verify it
        // at most once per verification window for unchanged committed state.
        let memo = VerificationMemo::new(view, session.digest, self.required_capabilities);
        if view.now_ns >= session.proof.transcript.expires_at_ns
            || !VerificationMemo::reusable(self.proved.get(), memo)
        {
            self.proved.set(None);
            self.verify_host(view, &session.proof, session.nonce)?;
            self.proved.set(memo);
        }
        let expires = view
            .now_ns
            .checked_add(60_000_000_000)
            .ok_or("delivery proof time overflow")?
            .min(session.proof.transcript.expires_at_ns);
        http_proof::RequestProof::sign(
            session.proof.transcript.session_id,
            session.digest,
            &http_proof::Request::new(method, target, body).map_err(fail)?,
            view.now_ns,
            expires,
            view.key,
        )
        .map_err(fail)?
        .encode()
        .map_err(fail)
    }
}
#[cfg(test)]
mod verification_memo_tests {
    use super::*;
    fn memo(revision: u64, at: u64) -> Option<VerificationMemo> {
        (revision != 0).then_some(VerificationMemo {
            state_revision: revision,
            subject: [1; 32],
            capabilities: 1,
            generation: (0, 1, [0; 32]),
            verified_at_ns: at,
        })
    }
    #[test]
    fn verification_is_reused_only_for_the_same_state_within_the_window() {
        let held = memo(7, 1_000);
        assert!(VerificationMemo::reusable(held, memo(7, 1_000)));
        assert!(VerificationMemo::reusable(
            held,
            memo(7, 1_000 + VERIFICATION_REUSE_NS - 1)
        ));
        assert!(!VerificationMemo::reusable(
            held,
            memo(7, 1_000 + VERIFICATION_REUSE_NS)
        ));
        assert!(
            !VerificationMemo::reusable(held, memo(7, 999)),
            "clock went backwards"
        );
        assert!(
            !VerificationMemo::reusable(held, memo(8, 1_000)),
            "committed state changed"
        );
        assert!(!VerificationMemo::reusable(held, None), "unknown revision");
        assert!(!VerificationMemo::reusable(None, memo(7, 1_000)));
        for changed in [
            VerificationMemo {
                subject: [2; 32],
                ..held.unwrap()
            },
            VerificationMemo {
                capabilities: 3,
                ..held.unwrap()
            },
            VerificationMemo {
                generation: (1, 1, [0; 32]),
                ..held.unwrap()
            },
        ] {
            assert!(!VerificationMemo::reusable(held, Some(changed)));
        }
    }
}
